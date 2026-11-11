use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value as Json, json};

use crate::config::{Config, ImageBackendKind};
use crate::image_models::{self, ImageModel};

const OPENROUTER_IMAGES_URL: &str = "https://openrouter.ai/api/v1/images";

/// A backend returns encoded image bytes independently of where generation happens.
/// Local implementations can be added without changing the `make-image` operation.
pub trait ImageBackend {
    /// Generates an image, reporting slow steps through `progress`.
    fn generate(&self, prompt: &str, progress: &mut dyn FnMut(String)) -> Result<GeneratedImage, String>;
}

pub struct GeneratedImage {
    pub bytes: Vec<u8>,
}

/// The backend `config` selects.
pub fn backend(config: &Config) -> Result<Box<dyn ImageBackend>, String> {
    match config.image_backend {
        ImageBackendKind::Local => {
            let model = image_models::resolve_installed(&config.local_image_model)?;
            Ok(Box::new(LocalImageBackend::new(model)?))
        }
        ImageBackendKind::OpenRouter => Ok(Box::new(OpenRouterImageBackend::from_environment(&config.image_model)?)),
    }
}

pub struct OpenRouterImageBackend {
    model: String,
    api_key: String,
}

impl OpenRouterImageBackend {
    pub fn from_environment(model: &str) -> Result<Self, String> {
        let api_key = std::env::var("OPENROUTER_API_KEY").map_err(|_| {
            "OPENROUTER_API_KEY is not set; set it, or generate locally: `forgeflow image` lists local models".to_string()
        })?;
        if api_key.trim().is_empty() {
            return Err("OPENROUTER_API_KEY is empty".into());
        }
        if model.trim().is_empty() {
            return Err("image_model is empty in _forgeflow/config.json".into());
        }
        Ok(Self {
            model: model.to_string(),
            api_key,
        })
    }
}

impl ImageBackend for OpenRouterImageBackend {
    fn generate(&self, prompt: &str, _progress: &mut dyn FnMut(String)) -> Result<GeneratedImage, String> {
        let body = json!({
            "model": self.model,
            "prompt": prompt,
            "aspect_ratio": "1:1",
            "n": 1,
        });
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_global(Some(Duration::from_secs(300)))
            .http_status_as_error(false)
            .build()
            .into();
        let mut response = agent
            .post(OPENROUTER_IMAGES_URL)
            .header("Authorization", &format!("Bearer {}", self.api_key))
            .send_json(&body)
            .map_err(|e| format!("could not reach OpenRouter image API: {e}"))?;
        let status = response.status();
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("could not read OpenRouter response: {e}"))?;
        if !status.is_success() {
            let excerpt: String = text.chars().take(2000).collect();
            let suffix = if text.chars().count() > 2000 { "…" } else { "" };
            return Err(format!("OpenRouter image API returned {status}: {excerpt}{suffix}"));
        }
        let json: Json = serde_json::from_str(&text)
            .map_err(|e| format!("OpenRouter returned invalid JSON: {e}"))?;
        let data = json
            .get("data")
            .and_then(Json::as_array)
            .ok_or("OpenRouter image response has no data array")?;
        let first = data.first().ok_or("OpenRouter returned no image")?;
        let encoded = first
            .get("b64_json")
            .and_then(Json::as_str)
            .ok_or("OpenRouter image response has no base64 image data")?;
        let bytes = STANDARD
            .decode(encoded)
            .map_err(|e| format!("OpenRouter returned invalid base64 image data: {e}"))?;
        Ok(GeneratedImage { bytes })
    }
}

/// Runs stable-diffusion.cpp's `sd-cli` once per image, so the diffusion model is only
/// resident while generating rather than alongside llama-server for the whole session.
pub struct LocalImageBackend {
    binary: PathBuf,
    model: ImageModel,
}

/// Most reference images an edit takes; Qwen Image Edit is trained on up to three or four.
pub const MAX_EDIT_INPUTS: usize = 4;

impl LocalImageBackend {
    pub fn new(model: ImageModel) -> Result<Self, String> {
        let binary = crate::toolchain::ensure(crate::toolchain::Tool::SdCli)?;
        Ok(Self { binary, model })
    }

    /// Edits `inputs` as `instruction` asks. The output keeps the first input's aspect
    /// ratio at about the model's default pixel count.
    pub fn edit(&self, instruction: &str, inputs: &[PathBuf], progress: &mut dyn FnMut(String)) -> Result<GeneratedImage, String> {
        let first = inputs.first().ok_or("edit-image needs at least one input image")?;
        if inputs.len() > MAX_EDIT_INPUTS {
            return Err(format!("edit-image takes at most {MAX_EDIT_INPUTS} images, got {}", inputs.len()));
        }
        let (width, height) =
            image::image_dimensions(first).map_err(|e| format!("could not read {}: {e}", first.display()))?;
        let size = fit_area(width, height, self.model.width * self.model.height);
        let m = &self.model;
        progress(format!(
            "Editing {} with {} locally ({} steps at {}×{})…",
            first.file_name().unwrap_or_default().to_string_lossy(),
            m.id,
            m.steps,
            size.0,
            size.1
        ));
        self.run(instruction, inputs, size, progress)
    }

    fn run(&self, prompt: &str, inputs: &[PathBuf], size: (u32, u32), progress: &mut dyn FnMut(String)) -> Result<GeneratedImage, String> {
        let started = std::time::Instant::now();
        let output = std::env::temp_dir().join(format!("forgeflow-sd-{}.png", uuid::Uuid::now_v7()));
        let result = Command::new(&self.binary)
            .args(self.arguments(prompt, &output, inputs, size))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .and_then(|child| {
                // Tracked so an interrupted ForgeFlow stops the job rather than leaving it
                // running on the GPU.
                let pid = child.id();
                crate::server::track_child(pid);
                let reaper = crate::reaper::watch(pid);
                let output = child.wait_with_output();
                crate::server::untrack_child(pid);
                drop(reaper);
                output
            })
            .map_err(|e| format!("could not start {}: {e}", self.binary.display()));
        let bytes = result.and_then(|run| {
            if !run.status.success() {
                let log = String::from_utf8_lossy(&run.stderr) + String::from_utf8_lossy(&run.stdout);
                let lines: Vec<&str> = log.lines().collect();
                let tail = lines[lines.len().saturating_sub(15)..].join("\n");
                return Err(format!("sd-cli exited ({}); last log lines:\n{tail}", run.status));
            }
            std::fs::read(&output).map_err(|e| format!("sd-cli wrote no image to {}: {e}", output.display()))
        });
        let _ = std::fs::remove_file(&output);
        let bytes = bytes?;
        progress(format!("Done in {:.0}s.", started.elapsed().as_secs_f64()));
        Ok(GeneratedImage { bytes })
    }

    fn arguments(&self, prompt: &str, output: &Path, inputs: &[PathBuf], size: (u32, u32)) -> Vec<std::ffi::OsString> {
        let m = &self.model;
        let mut args: Vec<std::ffi::OsString> = vec![];
        let mut prompt = prompt.to_string();
        for file in &m.files {
            let path = m.file_path(file);
            if file.flag == image_models::LORA_FLAG {
                // sd-cli loads LoRAs named in the prompt from one folder.
                let name = path.file_stem().unwrap_or_default().to_string_lossy();
                prompt.push_str(&format!(" <lora:{name}:1>"));
                if !args.iter().any(|a| a == "--lora-model-dir") {
                    let dir = path.parent().unwrap_or(Path::new("."));
                    args.extend(["--lora-model-dir".into(), dir.into()]);
                }
                continue;
            }
            args.extend([file.flag.clone().into(), path.into()]);
        }
        for input in inputs {
            args.extend(["-r".into(), input.into()]);
        }
        for (flag, value) in [
            ("-p", prompt),
            ("--steps", m.steps.to_string()),
            ("--cfg-scale", m.cfg_scale.to_string()),
            ("-W", size.0.to_string()),
            ("-H", size.1.to_string()),
            // sd-cli's default seed is fixed; a negative one picks a random seed.
            ("--seed", "-1".to_string()),
        ] {
            args.extend([flag.into(), value.into()]);
        }
        args.extend(m.extra_args.iter().map(Into::into));
        args.extend(["-o".into(), output.as_os_str().to_owned()]);
        args
    }
}

impl ImageBackend for LocalImageBackend {
    fn generate(&self, prompt: &str, progress: &mut dyn FnMut(String)) -> Result<GeneratedImage, String> {
        let m = &self.model;
        // Local generation takes minutes, so say what is happening before it starts.
        progress(format!("Generating with {} locally ({} steps at {}×{})…", m.id, m.steps, m.width, m.height));
        self.run(prompt, &[], (m.width, m.height), progress)
    }
}

/// `width`×`height` scaled to about `area` pixels, each side a multiple of 16 as the
/// diffusion models' latents require.
pub fn fit_area(width: u32, height: u32, area: u32) -> (u32, u32) {
    let scale = (area as f64 / (width.max(1) as f64 * height.max(1) as f64)).sqrt();
    let side = |v: u32| (((v as f64 * scale) / 16.0).round() as u32).max(1) * 16;
    (side(width), side(height))
}

/// GPU memory `model` needs while running: with `--offload-to-cpu` only the largest
/// component is resident at once, otherwise all of them, plus room for activations.
pub fn gpu_bytes_needed(model: &ImageModel) -> u64 {
    // Video holds many frames at once: 13 GB of activations for 81 frames at 832×480.
    const ACTIVATIONS: u64 = 3_000_000_000;
    const VIDEO_ACTIVATIONS: u64 = 13_000_000_000;
    let activations = if model.task == image_models::Task::Video { VIDEO_ACTIVATIONS } else { ACTIVATIONS };
    let weights = if model.extra_args.iter().any(|a| a == "--offload-to-cpu") {
        model.files.iter().map(|f| f.size_bytes).max().unwrap_or(0)
    } else {
        model.size_bytes()
    };
    weights + activations
}

pub fn save_image(path: &Path, image: &GeneratedImage) -> Result<std::path::PathBuf, String> {
    let decoded = image::load_from_memory(&image.bytes)
        .map_err(|e| format!("could not decode generated image: {e}"))?;
    let output_path = path.to_path_buf();
    let parent = output_path
        .parent()
        .ok_or_else(|| format!("invalid output path {}", output_path.display()))?;
    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    let file_name = output_path
        .file_name()
        .ok_or_else(|| format!("invalid output path {}", output_path.display()))?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        file_name.to_string_lossy(),
        uuid::Uuid::now_v7()
    ));
    let file = std::fs::File::create(&temporary)
        .map_err(|e| format!("{}: {e}", temporary.display()))?;
    if let Err(error) = decoded.write_to(&mut std::io::BufWriter::new(file), image::ImageFormat::Png) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("could not encode PNG {}: {error}", temporary.display()));
    }
    if let Err(error) = std::fs::rename(&temporary, &output_path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("could not save {}: {error}", output_path.display()));
    }
    Ok(output_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_arguments_pass_each_file_then_settings() {
        let model = image_models::find("chroma1-hd").unwrap();
        let backend = LocalImageBackend { binary: "sd-cli".into(), model };
        let joined = backend
            .arguments("a lantern", Path::new("/tmp/out.png"), &[], (1024, 1024))
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(joined.starts_with("--diffusion-model "));
        assert!(joined.contains("chroma1-hd/Chroma1-HD-Q4_K_M.gguf --t5xxl "));
        assert!(joined.contains("chroma1-hd/ae.safetensors -p a lantern --steps 26 --cfg-scale 4 -W 1024 -H 1024 --seed -1"));
        assert!(joined.ends_with("--model-args chroma_use_dit_mask=false -o /tmp/out.png"));
    }

    #[test]
    fn edit_arguments_load_the_lora_from_its_folder_and_pass_references() {
        let model = image_models::find("qwen-image-edit-2511").unwrap();
        let backend = LocalImageBackend { binary: "sd-cli".into(), model };
        let args: Vec<String> = backend
            .arguments("add rain", Path::new("/tmp/out.png"), &[PathBuf::from("a.png"), PathBuf::from("b.png")], (1184, 880))
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let joined = args.join(" ");
        assert!(!args.iter().any(|a| a == "lora"));
        assert!(joined.contains("--lora-model-dir "));
        assert!(joined.contains("-r a.png -r b.png -p add rain <lora:Qwen-Image-Edit-2511-Lightning-4steps-V1.0-bf16:1> --steps 4"));
        assert!(joined.contains("-W 1184 -H 880"));
        assert!(joined.contains("--llm_vision "));
    }

    #[test]
    fn edits_keep_the_aspect_ratio_at_the_model_area() {
        assert_eq!(fit_area(1024, 1024, 1024 * 1024), (1024, 1024));
        assert_eq!(fit_area(1920, 1080, 1024 * 1024), (1360, 768));
        assert_eq!(fit_area(600, 800, 1024 * 1024), (880, 1184));
    }
}

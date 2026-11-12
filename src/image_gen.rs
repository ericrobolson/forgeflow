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

/// Updates from an interactive image-edit job.
pub enum EditEvent { Progress(String), Preview(PathBuf) }

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

    /// Interactive edit with live sampler messages and decoded step previews.
    /// All generated files stay in the caller's edit workspace until Finish/Cancel.
    pub fn edit_streaming(&self, instruction: &str, inputs: &[PathBuf], work: &Path, live_previews: bool, update: &mut dyn FnMut(EditEvent)) -> Result<GeneratedImage, String> {
        self.edit_streaming_inner(instruction, inputs, None, work, live_previews, update)
    }

    /// Preserve the first input's unmasked latents throughout sampling, while keeping
    /// all inputs available as edit references. White mask pixels may change.
    pub fn inpaint_streaming(&self, instruction: &str, inputs: &[PathBuf], mask: &Path, strength: f32, work: &Path, live_previews: bool, update: &mut dyn FnMut(EditEvent)) -> Result<GeneratedImage, String> {
        self.edit_streaming_inner(instruction, inputs, Some((mask, strength)), work, live_previews, update)
    }

    fn edit_streaming_inner(&self, instruction: &str, inputs: &[PathBuf], inpaint: Option<(&Path, f32)>, work: &Path, live_previews: bool, update: &mut dyn FnMut(EditEvent)) -> Result<GeneratedImage, String> {
        use std::io::{Read, Write};
        use std::sync::mpsc;
        let first = inputs.first().ok_or("face edit needs a target image")?;
        if inputs.len() > MAX_EDIT_INPUTS { return Err("too many reference images".into()); }
        let (width, height) = image::image_dimensions(first).map_err(|e| e.to_string())?;
        if let Some((mask, strength)) = inpaint {
            if !strength.is_finite() || !(0.25..=1.0).contains(&strength) { return Err("denoising strength must be between 0.25 and 1".into()); }
            if image::image_dimensions(mask).map_err(|e| format!("could not read inpainting mask: {e}"))? != (width, height) {
                return Err("inpainting mask must match the target image dimensions".into());
            }
        }
        let size = fit_area(width, height, self.model.width * self.model.height);
        std::fs::create_dir_all(work).map_err(|e| e.to_string())?;
        let mut renderer_log = std::fs::File::create(work.join("renderer.log")).map_err(|e| e.to_string())?;
        let output = work.join("generated-final.png");
        let mut args = self.arguments(instruction, &output, inputs, size);
        if let Some((mask, strength)) = inpaint {
            // Keep the large edit weights file-backed instead of copying all of them
            // into RAM while the GPU holds its own working copy.
            if !args.iter().any(|arg| arg == "--mmap") { args.push("--mmap".into()); }
            // Avoid sd-cli's implicit centre crop: the compositor maps the complete
            // generated canvas back to the complete context crop.
            let init_path = work.join("sampling-target.png");
            let mask_path = work.join("sampling-mask.png");
            image::open(first).map_err(|e| e.to_string())?
                .resize_exact(size.0, size.1, image::imageops::FilterType::Lanczos3)
                .save(&init_path).map_err(|e| e.to_string())?;
            image::open(mask).map_err(|e| e.to_string())?
                .resize_exact(size.0, size.1, image::imageops::FilterType::Nearest)
                .save(&mask_path).map_err(|e| e.to_string())?;
            args.extend(["--init-img".into(), init_path.into(), "--mask".into(), mask_path.into(), "--strength".into(), strength.to_string().into()]);
        }
        if live_previews {
            args.extend(["--preview".into(), "vae".into(), "--preview-path".into(), work.join("step-%03d.png").into(), "--preview-interval".into(), "1".into()]);
        }
        update(EditEvent::Progress(format!("Loading {} for a {}-step edit…", self.model.id, self.model.steps)));
        let mut child = Command::new(&self.binary).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .spawn().map_err(|e| format!("could not start image edit: {e}"))?;
        let pid = child.id();
        crate::server::track_child(pid);
        let reaper = crate::reaper::watch(pid);
        let (sender, receiver) = mpsc::channel();
        let mut readers = Vec::new();
        let streams: Vec<Box<dyn Read + Send>> = vec![Box::new(child.stdout.take().unwrap()), Box::new(child.stderr.take().unwrap())];
        for mut stream in streams {
            let sender = sender.clone();
            readers.push(std::thread::spawn(move || {
                let mut buffer = [0; 4096];
                while let Ok(count) = stream.read(&mut buffer) {
                    if count == 0 || sender.send(buffer[..count].to_vec()).is_err() { break; }
                }
            }));
        }
        drop(sender);
        let mut log = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut last_step = None;
        let result = (|| {
            let status = loop {
                if let Ok(bytes) = receiver.recv_timeout(Duration::from_millis(100)) {
                    renderer_log.write_all(&bytes).map_err(|e| e.to_string())?;
                    log.extend(bytes);
                    if log.len() > 64 * 1024 { log.drain(..log.len() - 32 * 1024); }
                    let text = String::from_utf8_lossy(&log);
                    let step = text.split(['\r', '\n']).rev().find_map(|line| sampling_step(line, self.model.steps));
                    if let Some((step, total)) = step.filter(|step| last_step != Some(*step)) {
                        update(EditEvent::Progress(format!("Rendering: {step}/{total} steps (~{}%)", step * 100 / total)));
                        last_step = Some((step, total));
                    }
                }
                if live_previews {
                    let mut previews: Vec<_> = std::fs::read_dir(work).map_err(|e| e.to_string())?.filter_map(Result::ok)
                        .map(|entry| entry.path()).filter(|path| path.file_name().is_some_and(|name| name.to_string_lossy().starts_with("step-")) && path.extension().is_some_and(|ext| ext == "png")).collect();
                    previews.sort();
                    for path in previews {
                        if !seen.contains(&path) && image::open(&path).is_ok() {
                            seen.insert(path.clone());
                            update(EditEvent::Preview(path));
                        }
                    }
                }
                if let Some(status) = child.try_wait().map_err(|e| e.to_string())? { break status; }
            };
            for reader in readers.drain(..) { let _ = reader.join(); }
            for bytes in receiver.try_iter() {
                renderer_log.write_all(&bytes).map_err(|e| e.to_string())?;
                log.extend(bytes);
            }
            if !status.success() {
                let text = String::from_utf8_lossy(&log);
                return Err(format!("image edit failed: {}", text.lines().rev().take(12).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")));
            }
            let bytes = std::fs::read(&output).map_err(|e| format!("image edit produced no output: {e}"))?;
            Ok(GeneratedImage { bytes })
        })();
        if result.is_err() { let _ = child.kill(); }
        let _ = child.wait();
        for reader in readers { let _ = reader.join(); }
        crate::server::untrack_child(pid);
        drop(reaper);
        result
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

fn sampling_step(line: &str, total: u32) -> Option<(u32, u32)> {
    if line.contains("generating image:") { return None; }
    line.split_whitespace().find_map(|word| {
        let word = word.trim_matches(|c: char| !c.is_ascii_digit() && c != '/');
        let (step, steps) = word.split_once('/')?;
        let step: u32 = step.parse().ok()?;
        let steps: u32 = steps.parse().ok()?;
        // Img2img strength can shorten the configured sampling schedule.
        (steps > 0 && steps <= total && step <= steps).then_some((step, steps))
    })
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
    fn progress_accepts_the_shortened_inpainting_schedule() {
        assert_eq!(sampling_step("2/3 - 0.4it/s", 4), Some((2, 3)));
        assert_eq!(sampling_step("4/4", 4), Some((4, 4)));
        assert_eq!(sampling_step("5/3", 4), None);
        assert_eq!(sampling_step("0/0", 4), None);
        assert_eq!(sampling_step("Loading 12/60 tensors", 4), None);
        assert_eq!(sampling_step("[I] generating image: 1/1 - seed 42", 4), None);
    }

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

    #[cfg(unix)]
    #[test]
    fn streaming_edit_reports_previews_and_renderer_failures() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("forgeflow-stream-test-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
        }
        let _cleanup = Cleanup(root.clone());
        let input = root.join("target with spaces.png");
        image::RgbaImage::from_pixel(16, 16, image::Rgba([50, 100, 150, 255])).save(&input).unwrap();
        let binary = root.join("fake-sd-cli");
        std::fs::write(&binary, r#"#!/bin/sh
while [ "$#" -gt 0 ]; do
    case "$1" in
        -o) output=$2; shift ;;
        -r) source=$2; shift ;;
        --init-img) init=$2; shift ;;
        --mask) mask=$2; shift ;;
        --strength) strength=$2; shift ;;
    esac
    shift
done
if [ -n "$mask" ]; then
    [ -f "$init" ] && [ -f "$mask" ] && [ "$strength" = "0.75" ] || exit 9
    printf '%s\n' "$mask" > "$(dirname "$output")/mask-used.txt"
fi
cp "$source" "$(dirname "$output")/step-001.png"
printf '1/4\n' >&2
sleep 0.3
cp "$source" "$output"
"#).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        let backend = LocalImageBackend { binary: binary.clone(), model: image_models::find("qwen-image-edit-2511").unwrap() };
        let work = root.join("render");
        let mut previews = Vec::new();
        let mut progress = Vec::new();
        let result = backend.edit_streaming("keep the target style", &[input.clone()], &work, true, &mut |event| match event {
            EditEvent::Preview(path) => previews.push(path),
            EditEvent::Progress(message) => progress.push(message),
        }).unwrap();
        assert!(image::load_from_memory(&result.bytes).is_ok());
        assert_eq!(previews, vec![work.join("step-001.png")]);
        assert!(progress.iter().any(|message| message.contains("1/4 steps")));
        // Previews remain available to the window after the renderer exits.
        assert!(previews[0].is_file());
        let mask = root.join("mask with spaces.png");
        image::GrayImage::from_pixel(16, 16, image::Luma([255])).save(&mask).unwrap();
        let masked_work = root.join("masked-render");
        backend.inpaint_streaming("keep the target style", &[input.clone()], &mask, 0.75, &masked_work, true, &mut |_| {}).unwrap();
        assert_eq!(std::fs::read_to_string(masked_work.join("mask-used.txt")).unwrap().trim(), masked_work.join("sampling-mask.png").to_str().unwrap());
        assert_eq!(image::image_dimensions(masked_work.join("sampling-target.png")).unwrap(), (1024, 1024));
        assert_eq!(image::image_dimensions(masked_work.join("sampling-mask.png")).unwrap(), (1024, 1024));
        image::GrayImage::new(8, 8).save(&mask).unwrap();
        assert!(backend.inpaint_streaming("test", &[input.clone()], &mask, 0.75, &root.join("invalid"), false, &mut |_| {}).is_err());
        assert!(!root.join("invalid").exists());
        std::fs::write(&binary, "#!/bin/sh\necho 'renderer test failure' >&2\nexit 7\n").unwrap();
        let error = backend.edit_streaming("keep the target style", &[input], &root.join("failed"), false, &mut |_| {}).err().expect("renderer should fail");
        assert!(error.contains("renderer test failure"), "{error}");
    }
}

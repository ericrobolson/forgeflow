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
    fn generate(&self, prompt: &str) -> Result<GeneratedImage, String>;
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
        let api_key = std::env::var("OPENROUTER_API_KEY")
            .map_err(|_| "OPENROUTER_API_KEY is not set".to_string())?;
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
    fn generate(&self, prompt: &str) -> Result<GeneratedImage, String> {
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

impl LocalImageBackend {
    pub fn new(model: ImageModel) -> Result<Self, String> {
        let binary = find_sd_cli().ok_or(
            "sd-cli not found. Build stable-diffusion.cpp in ~/local-models/stable-diffusion.cpp, or set FORGEFLOW_SD_CLI",
        )?;
        Ok(Self { binary, model })
    }

    fn arguments(&self, prompt: &str, output: &Path) -> Vec<std::ffi::OsString> {
        let m = &self.model;
        let mut args: Vec<std::ffi::OsString> = vec![];
        for file in &m.files {
            args.extend([file.flag.clone().into(), m.file_path(file).into()]);
        }
        for (flag, value) in [
            ("-p", prompt.to_string()),
            ("--steps", m.steps.to_string()),
            ("--cfg-scale", m.cfg_scale.to_string()),
            ("-W", m.width.to_string()),
            ("-H", m.height.to_string()),
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
    fn generate(&self, prompt: &str) -> Result<GeneratedImage, String> {
        let m = &self.model;
        // Local generation takes minutes, so say what is happening before it starts.
        eprintln!("Generating with {} locally ({} steps at {}×{})…", m.id, m.steps, m.width, m.height);
        let output = std::env::temp_dir().join(format!("forgeflow-sd-{}.png", uuid::Uuid::now_v7()));
        let result = Command::new(&self.binary)
            .args(self.arguments(prompt, &output))
            .stdin(Stdio::null())
            .output()
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
        Ok(GeneratedImage { bytes: bytes? })
    }
}

fn find_sd_cli() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("FORGEFLOW_SD_CLI") {
        return Some(path.into());
    }
    let built = crate::models::models_dir().join("stable-diffusion.cpp/build/bin/sd-cli");
    if built.is_file() {
        return Some(built);
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join("sd-cli"))
        .find(|candidate| candidate.is_file())
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
            .arguments("a lantern", Path::new("/tmp/out.png"))
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(joined.starts_with("--diffusion-model "));
        assert!(joined.contains("chroma1-hd/Chroma1-HD-Q4_K_M.gguf --t5xxl "));
        assert!(joined.contains("chroma1-hd/ae.safetensors -p a lantern --steps 26 --cfg-scale 4 -W 1024 -H 1024 --seed -1"));
        assert!(joined.ends_with("--model-args chroma_use_dit_mask=false -o /tmp/out.png"));
    }
}

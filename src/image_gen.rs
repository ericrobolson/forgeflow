use std::{path::Path, time::Duration};

use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value as Json, json};

const OPENROUTER_IMAGES_URL: &str = "https://openrouter.ai/api/v1/images";

/// A backend returns encoded image bytes independently of where generation happens.
/// Local implementations can be added without changing the `make-image` operation.
pub trait ImageBackend {
    fn generate(&self, prompt: &str) -> Result<GeneratedImage, String>;
}

pub struct GeneratedImage {
    pub bytes: Vec<u8>,
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

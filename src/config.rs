use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const CONFIG_FILE: &str = "config.json";

/// Project settings in `_forgeflow/config.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The local model the agent loop runs, by catalog or folder ID.
    pub model: Option<String>,
    /// An OpenAI-compatible server to use instead of starting llama-server,
    /// such as `http://127.0.0.1:11434/v1` for Ollama.
    pub endpoint: Option<String>,
    /// The model name sent to `endpoint`.
    pub endpoint_model: Option<String>,
    /// Image generation model ID, such as `recraft/recraft-v4.1-flash`.
    pub image_model: String,
    pub ctx: u64,
    pub port: u16,
    pub max_steps: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: None,
            endpoint: None,
            endpoint_model: None,
            image_model: "recraft/recraft-v4.1-flash".into(),
            ctx: 8192,
            port: 8043,
            max_steps: 24,
        }
    }
}

impl Config {
    fn path(folder: &Path) -> PathBuf {
        folder.join(CONFIG_FILE)
    }

    pub fn load(folder: &Path) -> Result<Self, String> {
        let path = Self::path(folder);
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        serde_json::from_str(&text).map_err(|e| format!("invalid {}: {e}", path.display()))
    }

    pub fn save(&self, folder: &Path) -> Result<(), String> {
        std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(Self::path(folder), json + "\n").map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_take_defaults() {
        let config: Config = serde_json::from_str(r#"{"model":"qwen3-8b-q4km"}"#).unwrap();
        assert_eq!(config.model.as_deref(), Some("qwen3-8b-q4km"));
        assert_eq!(config.ctx, Config::default().ctx);
    }
}

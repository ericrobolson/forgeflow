use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::config::{Config, ImageBackendKind};
use crate::models::{self, gb, models_dir};

/// A diffusion model run locally by stable-diffusion.cpp's `sd-cli`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ImageModel {
    pub id: String,
    pub name: String,
    pub license: String,
    pub tags: Vec<String>,
    pub files: Vec<ImageFile>,
    pub steps: u32,
    pub cfg_scale: f32,
    pub width: u32,
    pub height: u32,
    /// Further `sd-cli` arguments the model needs, such as its sampler.
    #[serde(default)]
    pub extra_args: Vec<String>,
}

/// One weights file and the `sd-cli` flag that loads it, such as `--vae`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ImageFile {
    pub flag: String,
    pub repo: String,
    /// Path within the repo; installed under its file name.
    pub path: String,
    pub size_bytes: u64,
    pub sha256: String,
}

impl ImageModel {
    pub fn size_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size_bytes).sum()
    }

    pub fn file_path(&self, file: &ImageFile) -> PathBuf {
        let name = Path::new(&file.path).file_name().expect("catalog paths name a file");
        models_dir().join(&self.id).join(name)
    }

    pub fn is_installed(&self) -> bool {
        self.files.iter().all(|f| self.file_path(f).exists())
    }

    fn is_partial(&self) -> bool {
        self.files
            .iter()
            .any(|f| self.file_path(f).exists() || models::partial_path(&self.file_path(f)).exists())
    }
}

impl ImageFile {
    fn url(&self) -> String {
        format!("https://huggingface.co/{}/resolve/main/{}", self.repo, self.path)
    }
}

pub fn catalog() -> Vec<ImageModel> {
    serde_json::from_str(include_str!("../catalog/image_models.json"))
        .expect("the embedded image model catalog is valid JSON")
}

pub fn find(id: &str) -> Option<ImageModel> {
    catalog().into_iter().find(|m| m.id == id)
}

/// Downloads each of an image model's files that is not installed yet.
pub fn pull(id: &str, progress: &mut dyn Write) -> Result<PathBuf, String> {
    let model = find(id).ok_or_else(|| format!("`{id}` is not in the image catalog; run `image_models`"))?;
    for file in &model.files {
        let path = model.file_path(file);
        if path.exists() {
            continue;
        }
        let label = format!("{} {} ({}) from {}", model.name, file.flag, gb(file.size_bytes), file.repo);
        models::download(id, &label, &file.url(), file.size_bytes, &file.sha256, &path, progress)?;
    }
    let dir = models_dir().join(&model.id);
    writeln!(progress, "{id} is installed in {}", dir.display()).ok();
    Ok(dir)
}

/// An installed image model, or an error saying how to get it.
pub fn resolve_installed(id: &str) -> Result<ImageModel, String> {
    let model = find(id).ok_or_else(|| format!("unknown image model `{id}`; run `image_models` to list them"))?;
    if !model.is_installed() {
        return Err(format!("`{id}` is not downloaded yet; run `forgeflow pull {id}`"));
    }
    Ok(model)
}

/// Where `make-image` sends requests, as one line.
pub fn describe_selection(config: &Config) -> String {
    match config.image_backend {
        ImageBackendKind::OpenRouter => format!("OpenRouter · {}", config.image_model),
        ImageBackendKind::Local => format!("local · {}", config.local_image_model),
    }
}

pub fn render_table(config: &Config) -> String {
    let models = catalog();
    let id_width = models.iter().map(|m| m.id.len()).max().unwrap_or(0);
    let tags_width = models.iter().map(|m| m.tags.join(" ").len()).max().unwrap_or(0);
    let mut out = format!("Image generation: {}\n\n", describe_selection(config));
    out.push_str(&format!("  {:<id_width$} {:>7} {:>6}  {:<tags_width$} {}\n", "ID", "SIZE", "STEPS", "TAGS", "STATUS"));
    for model in &models {
        let mut status = vec![];
        if model.is_installed() {
            status.push("installed");
        } else if model.is_partial() {
            status.push("partial");
        }
        if config.image_backend == ImageBackendKind::Local && config.local_image_model == model.id {
            status.push("selected");
        }
        out.push_str(&format!(
            "  {:<id_width$} {:>7} {:>6}  {:<tags_width$} {}\n",
            model.id,
            gb(model.size_bytes()),
            model.steps,
            model.tags.join(" "),
            status.join(" "),
        ));
    }
    out.push_str(
        "\nDownload with `forgeflow pull <id>`, then select with `forgeflow image <id>`.\n\
         Use `forgeflow image openrouter` (or an OpenRouter model such as `recraft/recraft-v4.1-flash`) for OpenRouter.",
    );
    out
}

/// Selects where `make-image` generates: `openrouter`, an OpenRouter model ID (it contains
/// a `/`), `local` for the last local model, or a local image model ID. Returns the new
/// selection, described.
pub fn select(folder: &Path, choice: &str) -> Result<String, String> {
    let mut config = Config::load(folder)?;
    let choice = choice.trim();
    match choice {
        "openrouter" => config.image_backend = ImageBackendKind::OpenRouter,
        _ if choice.contains('/') => {
            config.image_backend = ImageBackendKind::OpenRouter;
            config.image_model = choice.to_string();
        }
        _ => {
            let id = if choice == "local" { config.local_image_model.clone() } else { choice.to_string() };
            resolve_installed(&id)?;
            config.image_backend = ImageBackendKind::Local;
            config.local_image_model = id;
        }
    }
    config.save(folder)?;
    Ok(describe_selection(&config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_files_have_flags_and_checksums() {
        for model in catalog() {
            assert!(model.files.iter().any(|f| f.flag == "--diffusion-model"), "{}", model.id);
            for file in &model.files {
                assert_eq!(file.sha256.len(), 64, "{} {}", model.id, file.path);
            }
        }
    }

    #[test]
    fn selecting_openrouter_models_keeps_the_local_choice() {
        let folder = std::env::temp_dir().join(format!("forgeflow-image-select-{}", uuid::Uuid::now_v7()));
        let described = select(&folder, "black-forest-labs/flux.2-pro").unwrap();
        assert_eq!(described, "OpenRouter · black-forest-labs/flux.2-pro");
        let config = Config::load(&folder).unwrap();
        assert_eq!(config.local_image_model, Config::default().local_image_model);
        assert!(select(&folder, "no-such-model").is_err());
        std::fs::remove_dir_all(&folder).ok();
    }
}

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
    /// Whether the model makes images from a prompt or edits given ones.
    #[serde(default)]
    pub task: Task,
    pub tags: Vec<String>,
    pub files: Vec<ImageFile>,
    pub steps: u32,
    pub cfg_scale: f32,
    pub width: u32,
    pub height: u32,
    /// For video models: frames per clip and the frame rate they are made for.
    #[serde(default)]
    pub frames: u32,
    #[serde(default)]
    pub fps: u32,
    /// Further `sd-cli` arguments the model needs, such as its sampler.
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// For video models: how drafts differ from full quality, chosen by benchmarking speed
    /// against what still reads well. Without it, drafts are 512×512 at the full step count.
    #[serde(default)]
    pub draft: Option<DraftSettings>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct DraftSettings {
    pub width: u32,
    pub height: u32,
    pub steps: u32,
    /// Appended after the model's own arguments, so they win where both set a flag.
    #[serde(default)]
    pub extra_args: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Task {
    #[default]
    Generate,
    Edit,
    Video,
}

/// The `flag` of a file that is a LoRA: loaded from its folder and enabled in the prompt.
pub const LORA_FLAG: &str = "lora";

/// One weights file and the `sd-cli` flag that loads it, such as `--vae`, or `lora`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ImageFile {
    pub flag: String,
    pub repo: String,
    /// Path within the repo; installed under its file name.
    pub path: String,
    pub size_bytes: u64,
    pub sha256: String,
    /// Tensor shapes to relabel after download, for GGUF files whose converter stored a
    /// 5-D weight differently from what stable-diffusion.cpp expects. See `gguf`.
    #[serde(default)]
    pub reshape: std::collections::BTreeMap<String, Vec<u64>>,
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
        // Models share encoders and VAEs; link another model's copy instead of downloading it again.
        if let Some(existing) = installed_copy(file, &model.id) {
            std::fs::create_dir_all(path.parent().expect("model files sit in a folder")).map_err(|e| e.to_string())?;
            if std::fs::hard_link(&existing, &path).is_ok() {
                writeln!(progress, "{} {}: reusing {}", model.name, file.flag, existing.display()).ok();
                continue;
            }
        }
        let label = format!("{} {} ({}) from {}", model.name, file.flag, gb(file.size_bytes), file.repo);
        models::download(id, &label, &file.url(), file.size_bytes, &file.sha256, &path, progress)?;
    }
    for file in model.files.iter().filter(|f| !f.reshape.is_empty()) {
        let wanted: Vec<(String, Vec<u64>)> = file.reshape.iter().map(|(name, dims)| (name.clone(), dims.clone())).collect();
        let changed = crate::gguf::reshape_in_place(&model.file_path(file), &wanted)?;
        if changed > 0 {
            writeln!(progress, "Relabelled {changed} tensor shape(s) in {} for stable-diffusion.cpp", file.path).ok();
        }
    }
    let dir = models_dir().join(&model.id);
    writeln!(progress, "{id} is installed in {}", dir.display()).ok();
    Ok(dir)
}

/// An installed copy of `file` in another model's folder, matched by repo, path and checksum.
fn installed_copy(file: &ImageFile, except: &str) -> Option<PathBuf> {
    catalog()
        .into_iter()
        .filter(|m| m.id != except)
        .flat_map(|m| m.files.iter().filter(|f| f.repo == file.repo && f.path == file.path && f.sha256 == file.sha256).map(|f| m.file_path(f)).collect::<Vec<_>>())
        .find(|path| path.is_file())
}

/// An installed image model, or an error saying how to get it.
pub fn resolve_installed(id: &str) -> Result<ImageModel, String> {
    let model = find(id).ok_or_else(|| format!("unknown image model `{id}`; run `image_models` to list them"))?;
    if !model.is_installed() {
        return Err(format!("`{id}` is not downloaded yet; run `forgeflow pull {id}`"));
    }
    Ok(model)
}

/// The installed video model `make-video` uses, or an error saying how to get one.
pub fn video_model(config: &Config) -> Result<ImageModel, String> {
    let model = resolve_installed(&config.local_video_model)?;
    if model.task != Task::Video {
        return Err(format!("`{}` is not a video model; choose one with `forgeflow image <id>`", model.id));
    }
    Ok(model)
}

/// The installed edit model `edit-image` uses, or an error saying how to get one.
pub fn edit_model(config: &Config) -> Result<ImageModel, String> {
    let model = resolve_installed(&config.local_edit_model)?;
    if model.task != Task::Edit {
        return Err(format!("`{}` cannot edit images; choose an edit model with `forgeflow image <id>`", model.id));
    }
    Ok(model)
}

/// The settings `make-image` should run with. OpenRouter needs `OPENROUTER_API_KEY`;
/// without it, generation falls back to the local image model, or else the local edit model
/// (edit models also generate from text), when one is downloaded. The note says so.
pub fn effective_generation(config: &Config) -> (Config, Option<String>) {
    let has_key = std::env::var("OPENROUTER_API_KEY").is_ok_and(|key| !key.trim().is_empty());
    if config.image_backend != ImageBackendKind::OpenRouter || has_key {
        return (config.clone(), None);
    }
    let installed = |id: &str| find(id).is_some_and(|m| m.is_installed());
    let local = [config.local_image_model.as_str(), config.local_edit_model.as_str()]
        .into_iter()
        .find(|id| installed(id))
        .map(str::to_string);
    match local {
        Some(id) => {
            let mut fallback = config.clone();
            fallback.image_backend = ImageBackendKind::Local;
            fallback.local_image_model = id.clone();
            let note = format!("OPENROUTER_API_KEY is not set, so using local · {id} (select it with `/image {id}` to skip this note)");
            (fallback, Some(note))
        }
        None => (config.clone(), None),
    }
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
    let mut out = format!(
        "Image generation: {}\nImage editing: local · {}\nVideo: local · {} ({} quality)\n\n",
        describe_selection(config),
        config.local_edit_model,
        config.local_video_model,
        config.video_quality
    );
    out.push_str(&format!(
        "  {:<id_width$} {:<8} {:>7} {:>6}  {:<tags_width$} {}\n",
        "ID", "TASK", "SIZE", "STEPS", "TAGS", "STATUS"
    ));
    for model in &models {
        let mut status = vec![];
        if model.is_installed() {
            status.push("installed");
        } else if model.is_partial() {
            status.push("partial");
        }
        let selected = match model.task {
            Task::Generate => config.image_backend == ImageBackendKind::Local && config.local_image_model == model.id,
            Task::Edit => {
                config.local_edit_model == model.id
                    || (config.image_backend == ImageBackendKind::Local && config.local_image_model == model.id)
            }
            Task::Video => config.local_video_model == model.id,
        };
        if selected {
            status.push("selected");
        }
        out.push_str(&format!(
            "  {:<id_width$} {:<8} {:>7} {:>6}  {:<tags_width$} {}\n",
            model.id,
            match model.task {
                Task::Generate => "generate",
                Task::Edit => "edit",
                Task::Video => "video",
            },
            gb(model.size_bytes()),
            model.steps,
            model.tags.join(" "),
            status.join(" "),
        ));
    }
    out.push_str(
        "\nDownload with `forgeflow pull <id>`, then select with `forgeflow image <id>`; an edit model is used for both `make-image` and `edit-image`.\n\
         Use `forgeflow image openrouter` (or an OpenRouter model such as `recraft/recraft-v4.1-flash`) for OpenRouter.",
    );
    out
}

/// Selects where `make-image` generates: `openrouter`, an OpenRouter model ID (it contains
/// a `/`), `local` for the last local model, or a local image model ID. An edit model ID
/// selects it for `edit-image` and, as it also generates from text, for `make-image`.
/// Returns the new selection, described.
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
            if resolve_installed(&id)?.task == Task::Video {
                config.local_video_model = id.clone();
                config.save(folder)?;
                return Ok(format!("making videos with local · {id}"));
            }
            // An edit model also generates from text alone, so it serves both words.
            if resolve_installed(&id)?.task == Task::Edit {
                config.local_edit_model = id.clone();
                config.image_backend = ImageBackendKind::Local;
                config.local_image_model = id.clone();
                config.save(folder)?;
                return Ok(format!("generating and editing with local · {id}"));
            }
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
            assert!(model.files.iter().all(|f| f.flag == LORA_FLAG || f.flag.starts_with('-')), "{}", model.id);
            for file in &model.files {
                assert_eq!(file.sha256.len(), 64, "{} {}", model.id, file.path);
            }
        }
    }

    #[test]
    fn without_an_openrouter_key_generation_falls_back_to_an_installed_model() {
        if std::env::var_os("OPENROUTER_API_KEY").is_some() || !find("qwen-image-edit-2511").is_some_and(|m| m.is_installed()) {
            return;
        }
        let config = Config { local_image_model: "no-such-model".into(), ..Config::default() };
        let (effective, note) = effective_generation(&config);
        assert_eq!(effective.image_backend, ImageBackendKind::Local);
        assert_eq!(effective.local_image_model, "qwen-image-edit-2511");
        assert!(note.unwrap().contains("OPENROUTER_API_KEY is not set"));
    }

    #[test]
    fn selecting_openrouter_models_keeps_the_local_choice() {
        let folder = std::env::temp_dir().join(format!("forgeflow-image-select-{}", uuid::Uuid::now_v7()));
        let described = select(&folder, "black-forest-labs/flux.2-pro").unwrap();
        assert_eq!(described, "OpenRouter · black-forest-labs/flux.2-pro");
        let config = Config::load(&folder).unwrap();
        assert_eq!(config.local_image_model, Config::default().local_image_model);
        assert!(select(&folder, "no-such-model").is_err());
        // Edit models are only selectable once downloaded, as on a real machine.
        if find("qwen-image-edit-2511").is_some_and(|m| m.is_installed()) {
            assert_eq!(select(&folder, "qwen-image-edit-2511").unwrap(), "generating and editing with local · qwen-image-edit-2511");
            let config = Config::load(&folder).unwrap();
            assert_eq!(config.image_backend, ImageBackendKind::Local);
            assert_eq!((config.local_image_model.as_str(), config.local_edit_model.as_str()), ("qwen-image-edit-2511", "qwen-image-edit-2511"));
        }
        std::fs::remove_dir_all(&folder).ok();
    }
}

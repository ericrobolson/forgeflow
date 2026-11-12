//! One dataset format for the CLI, words, training, and local generation.
use crate::{
    config::{Config, ImageBackendKind},
    project::Project,
};
use image::ImageDecoder;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub const MANIFEST: &str = "lora.json";
pub const BASE: &str = "qwen-image";

struct DownloadProgress<'a> {
    callback: &'a mut dyn FnMut(String),
    line: Vec<u8>,
}
impl Write for DownloadProgress<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.line.extend_from_slice(bytes);
        if bytes.contains(&b'\n') {
            self.flush()?;
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let text = String::from_utf8_lossy(&self.line).trim().to_string();
        if !text.is_empty() {
            (self.callback)(text);
        }
        self.line.clear();
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Caption {
    Text(String),
    Tags(Vec<String>),
}
impl Caption {
    fn text(&self) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::Tags(tags) => tags.join(", "),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Training {
    pub steps: u32,
    pub rank: u32,
    pub learning_rate: f64,
    pub resolution: u32,
    pub save_every: u32,
    pub seed: u32,
}
impl Default for Training {
    fn default() -> Self {
        Self {
            steps: 1000,
            rank: 16,
            learning_rate: 0.0001,
            resolution: 512,
            save_every: 100,
            seed: 42,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dataset {
    pub version: u32,
    pub name: String,
    pub base_model: String,
    pub trigger: String,
    pub images: BTreeMap<String, Caption>,
    #[serde(default)]
    pub training: Training,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActiveLora {
    pub path: PathBuf,
    pub base_model: String,
    pub trigger: String,
    pub strength: f32,
}

fn child(root: &Path, relative: &str) -> Result<PathBuf, String> {
    if relative != relative.trim() || relative.is_empty() {
        return Err(
            "dataset paths must be nonempty and have no leading/trailing whitespace".into(),
        );
    }
    Project {
        root: root.to_path_buf(),
    }
    .resolve(relative)
}

fn folder(project: &Project, relative: &str) -> Result<PathBuf, String> {
    let path = project.resolve(relative)?;
    if path.file_name().is_some_and(|n| n == MANIFEST) {
        Ok(path
            .parent()
            .ok_or("manifest has no parent directory")?
            .to_path_buf())
    } else {
        Ok(path)
    }
}

fn load(root: &Path) -> Result<Dataset, String> {
    let path = child(root, MANIFEST)?;
    serde_json::from_slice(&fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?)
        .map_err(|e| format!("invalid {}: {e}", path.display()))
}

fn atomic_json(path: &Path, value: &impl Serialize, create: bool) -> Result<(), String> {
    let text = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    if create {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        file.write_all(&text)
            .and_then(|_| file.write_all(b"\n"))
            .map_err(|e| e.to_string())
    } else {
        let tmp = path.with_extension(format!("{}.tmp", uuid::Uuid::now_v7()));
        let result = (|| {
            fs::write(&tmp, [&text[..], b"\n"].concat()).map_err(|e| e.to_string())?;
            fs::rename(&tmp, path).map_err(|e| e.to_string())
        })();
        if result.is_err() {
            let _ = fs::remove_file(tmp);
        }
        result
    }
}

fn image_paths(root: &Path) -> Result<Vec<String>, String> {
    let images = child(root, "images")?;
    let mut paths = Vec::new();
    for entry in walkdir::WalkDir::new(&images).follow_links(false) {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if !path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            ["jpg", "jpeg", "png", "webp"].contains(&e.to_ascii_lowercase().as_str())
        }) {
            continue;
        }
        paths.push(
            path.strip_prefix(&images)
                .unwrap()
                .to_str()
                .ok_or("image filename is not UTF-8")?
                .replace('\\', "/"),
        );
    }
    paths.sort();
    Ok(paths)
}

fn relative(project: &Project, path: &Path) -> Result<String, String> {
    Ok(path
        .strip_prefix(project.root.canonicalize().map_err(|e| e.to_string())?)
        .map_err(|_| "dataset is outside the project")?
        .to_string_lossy()
        .into_owned())
}

pub fn init(project: &Project, relative_path: &str) -> Result<String, String> {
    let root = folder(project, relative_path)?;
    fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let manifest = child(&root, MANIFEST)?;
    if manifest.exists() {
        return Err(
            "lora.json already exists; use lora-scan to add images without replacing captions"
                .into(),
        );
    }
    fs::create_dir_all(child(&root, "images")?).map_err(|e| e.to_string())?;
    let name: String = root
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    let name = name.trim_matches('_').to_string();
    let name = if name.is_empty() {
        "subject".into()
    } else {
        name
    };
    let dataset = Dataset {
        version: 1,
        trigger: format!("ff_{name}"),
        name,
        base_model: BASE.into(),
        images: image_paths(&root)?
            .into_iter()
            .map(|p| (p, Caption::Text(String::new())))
            .collect(),
        training: Training::default(),
    };
    atomic_json(&manifest, &dataset, true)?;
    let instructions = child(&root, "README.txt")?;
    if !instructions.exists() {
        fs::OpenOptions::new().write(true).create_new(true).open(instructions).and_then(|mut f| f.write_all(b"Put training images in images/ (subfolders supported).\nRun forgeflow lora-scan <folder> to add filename keys to lora.json.\nFill each caption string, or use an array of phrase tags. Keys are relative to images/.\nExample: \"front.jpg\": [\"person\", \"front view\", \"daylight\"]\nThe trigger is prepended automatically when preparing training captions.\nRun forgeflow lora-check <folder>, then forgeflow train-lora <folder>.\nTraining is local and may download training weights and Python dependencies.\nWeights, logs and training configuration are kept under runs/.\nThese operations also work as words in a normal ForgeFlow session or .ff workflow. use-lora saves the selection across restarts; no startup options are needed.\nUse forgeflow lora <folder> 0.8 to select the trained adapter for local Qwen generation.\n")).map_err(|e| e.to_string())?;
    }
    relative(project, &manifest)
}

pub fn scan(project: &Project, path: &str) -> Result<String, String> {
    let root = folder(project, path)?;
    let mut dataset = load(&root)?;
    let mut added = 0;
    for name in image_paths(&root)? {
        if let std::collections::btree_map::Entry::Vacant(entry) = dataset.images.entry(name) {
            entry.insert(Caption::Text(String::new()));
            added += 1;
        }
    }
    if added != 0 {
        atomic_json(&child(&root, MANIFEST)?, &dataset, false)?;
    }
    Ok(format!(
        "Added {added} images; existing captions preserved. Manifest: {}",
        relative(project, &root.join(MANIFEST))?
    ))
}

pub fn validate(project: &Project, path: &str) -> Result<String, String> {
    let root = folder(project, path)?;
    let dataset = checked(&root)?;
    Ok(format!(
        "Valid Qwen dataset: {} images, trigger {}, {} steps at {} px.",
        dataset.images.len(),
        dataset.trigger,
        dataset.training.steps,
        dataset.training.resolution
    ))
}

fn checked(root: &Path) -> Result<Dataset, String> {
    let d = load(root)?;
    if d.version != 1 || d.base_model != BASE {
        return Err("supported dataset version is 1; base_model must be qwen-image".into());
    }
    if d.name.is_empty()
        || !d
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("name must contain only letters, numbers, hyphens and underscores".into());
    }
    if d.trigger.trim().is_empty() || d.trigger.contains(['\n', '\r', '<', '>']) {
        return Err("trigger must be a nonempty single-line phrase".into());
    }
    let t = &d.training;
    if t.steps == 0
        || !(1..=128).contains(&t.rank)
        || !(256..=1024).contains(&t.resolution)
        || t.resolution % 32 != 0
        || !t.learning_rate.is_finite()
        || !(0.0..=0.01).contains(&t.learning_rate)
        || t.learning_rate == 0.0
        || t.save_every == 0
    {
        return Err("invalid training settings: steps/save_every > 0, rank 1–128, resolution 256–1024 (multiple of 32), learning_rate > 0 and <= 0.01".into());
    }
    if d.images.is_empty() {
        return Err("no captioned images; add files to images/, run lora-scan, then fill the captions in lora.json".into());
    }
    let images = child(root, "images")?;
    let mut errors = Vec::new();
    for (name, caption) in &d.images {
        let result = (|| {
            let file = child(&images, name)?;
            if caption.text().trim().is_empty() {
                return Err("caption is empty".to_string());
            }
            if matches!(caption, Caption::Tags(tags) if tags.iter().any(|tag| tag.trim().is_empty()))
            {
                return Err("phrase tags must not be empty".into());
            }
            image::ImageReader::open(file)
                .map_err(|e| e.to_string())?
                .with_guessed_format()
                .map_err(|e| e.to_string())?
                .decode()
                .map_err(|e| e.to_string())?;
            Ok(())
        })();
        if let Err(e) = result {
            errors.push(format!("{name}: {e}"));
        }
    }
    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    Ok(d)
}

/// Freeze a normalized snapshot so edits to source files cannot change a running job.
pub fn prepare(project: &Project, path: &str) -> Result<PathBuf, String> {
    let root = folder(project, path)?;
    let d = checked(&root)?;
    let runs = child(&root, "runs")?;
    fs::create_dir_all(&runs).map_err(|e| e.to_string())?;
    let run = child(&root, &format!("runs/{}", uuid::Uuid::now_v7()))?;
    fs::create_dir_all(run.join("dataset")).map_err(|e| e.to_string())?;
    let images = child(&root, "images")?;
    for (index, (name, caption)) in d.images.iter().enumerate() {
        let stem = format!("{index:06}");
        let mut img = image::ImageReader::open(child(&images, name)?)
            .map_err(|e| e.to_string())?
            .with_guessed_format()
            .map_err(|e| e.to_string())?
            .decode()
            .map_err(|e| e.to_string())?;
        // Apply phone-photo orientation before captions and training are frozen.
        if let Ok(reader) = image::ImageReader::open(child(&images, name)?) {
            if let Ok(mut decoder) = reader
                .with_guessed_format()
                .map_err(|e| e.to_string())?
                .into_decoder()
            {
                if let Ok(orientation) = decoder.orientation() {
                    img.apply_orientation(orientation);
                }
            }
        }
        let limit = d.training.resolution * 2;
        if img.width() > limit || img.height() > limit {
            img = img.resize(limit, limit, image::imageops::FilterType::Lanczos3);
        }
        img.to_rgb8()
            .save(run.join("dataset").join(format!("{stem}.png")))
            .map_err(|e| e.to_string())?;
        let caption = caption.text();
        let caption = if caption.contains(&d.trigger) {
            caption
        } else {
            format!("{}, {caption}", d.trigger)
        };
        fs::write(run.join("dataset").join(format!("{stem}.txt")), caption)
            .map_err(|e| e.to_string())?;
    }
    atomic_json(&run.join(MANIFEST), &d, true)?;
    let config = serde_json::json!({"job":"extension","config":{"name":d.name,"process":[{
        "type":"sd_trainer", "device":"cuda:0", "training_folder":run.join("output"),
        "network":{"type":"lora","linear":d.training.rank,"linear_alpha":d.training.rank},
        "save":{"dtype":"float16","save_every":d.training.save_every,"max_step_saves_to_keep":2},
        "datasets":[{"folder_path":run.join("dataset"),"caption_ext":"txt","caption_dropout_rate":0.0,"shuffle_tokens":false,"cache_latents_to_disk":true,"resolution":[d.training.resolution]}],
        "train":{"batch_size":1,"steps":d.training.steps,"gradient_accumulation":1,"train_unet":true,"train_text_encoder":false,"cache_text_embeddings":true,"gradient_checkpointing":true,"noise_scheduler":"flowmatch","optimizer":"adamw8bit","lr":d.training.learning_rate,"dtype":"bf16","disable_sampling":true,"skip_first_sample":true,"seed":d.training.seed},
        "model":{"name_or_path":"Qwen/Qwen-Image","arch":"qwen_image","quantize":true,"qtype":"uint3|ostris/accuracy_recovery_adapters/qwen_image_torchao_uint3.safetensors","quantize_te":true,"qtype_te":"qfloat8","low_vram":true},
        "sample":{"sampler":"flowmatch","sample_every":d.training.save_every,"width":d.training.resolution,"height":d.training.resolution,"prompts":[format!("{} in a studio photograph",d.trigger)],"seed":d.training.seed,"guidance_scale":3,"sample_steps":20}
    }]},"meta":{"name":d.name,"version":"1.0"}});
    atomic_json(&run.join("trainer.json"), &config, true)?;
    Ok(run)
}

pub fn train(
    project: &Project,
    path: &str,
    progress: &mut dyn FnMut(String),
) -> Result<String, String> {
    let root = folder(project, path)?;
    let run = prepare(project, path)?;
    let d = load(&run)?;
    progress(format!("Training snapshot: {}", run.display()));
    if !crate::image_models::find(BASE).is_some_and(|model| model.is_installed()) {
        progress(
            "Preparing the local Qwen base (about 38 GiB plus inference encoders on first use)…"
                .into(),
        );
        let mut writer = DownloadProgress {
            callback: &mut *progress,
            line: Vec::new(),
        };
        crate::image_models::pull(BASE, &mut writer)?;
        writer.flush().map_err(|e| e.to_string())?;
    }
    let bridge = run.join("train.py");
    fs::write(&bridge, include_str!("../assets/lora_train.py")).map_err(|e| e.to_string())?;
    fs::write(
        run.join("worker.py"),
        include_str!("../assets/lora_worker.py"),
    )
    .map_err(|e| e.to_string())?;
    let mut log = fs::File::create(run.join("training.log")).map_err(|e| e.to_string())?;
    let mut command = Command::new("python3");
    command
        .arg(&bridge)
        .arg(run.join("trainer.json"))
        .arg(crate::models::models_dir());
    let mut process = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let pid = process.id();
    crate::server::track_child(pid);
    let reaper = crate::reaper::watch(pid);
    let (sender, receiver) = std::sync::mpsc::channel();
    for stream in [
        Box::new(process.stdout.take().unwrap()) as Box<dyn std::io::Read + Send>,
        Box::new(process.stderr.take().unwrap()),
    ] {
        let sender = sender.clone();
        std::thread::spawn(move || {
            let mut line = Vec::new();
            for byte in BufReader::new(stream).bytes() {
                let Ok(byte) = byte else { break };
                if byte == b'\n' || byte == b'\r' || line.len() >= 8192 {
                    if !line.is_empty()
                        && sender
                            .send(String::from_utf8_lossy(&line).into_owned())
                            .is_err()
                    {
                        return;
                    }
                    line.clear();
                }
                if byte != b'\n' && byte != b'\r' {
                    line.push(byte);
                }
            }
            if !line.is_empty() {
                let _ = sender.send(String::from_utf8_lossy(&line).into_owned());
            }
        });
    }
    drop(sender);
    let mut tail = std::collections::VecDeque::new();
    for line in receiver {
        let _ = writeln!(log, "{line}");
        progress(line.clone());
        tail.push_back(line);
        if tail.len() > 20 {
            tail.pop_front();
        }
    }
    let status = process.wait().map_err(|e| e.to_string());
    crate::server::untrack_child(pid);
    drop(reaper);
    if !status?.success() {
        return Err(format!(
            "LoRA training failed; snapshot and log retained at {}\n{}",
            run.display(),
            tail.into_iter().collect::<Vec<_>>().join("\n")
        ));
    }
    let weights = run
        .join("output")
        .join(&d.name)
        .join(format!("{}.safetensors", d.name));
    if !weights.is_file() {
        return Err(format!(
            "trainer produced no final LoRA at {}; see training.log",
            weights.display()
        ));
    }
    let adapter = ActiveLora {
        path: weights
            .strip_prefix(&root)
            .map_err(|_| "adapter is outside the dataset")?
            .to_path_buf(),
        base_model: d.base_model,
        trigger: d.trigger,
        strength: 1.0,
    };
    atomic_json(&child(&root, "adapter.json")?, &adapter, false)?;
    relative(project, &weights)
}

pub fn select(project: &Project, path: &str, strength: &str) -> Result<(), String> {
    let mut config = Config::load(&project.folder())?;
    if path == "off" {
        config.lora = None;
        return config.save(&project.folder());
    }
    let strength: f32 = strength
        .parse()
        .map_err(|_| "LoRA strength must be a number")?;
    if !strength.is_finite() || !(0.0..=2.0).contains(&strength) {
        return Err("LoRA strength must be between 0 and 2".into());
    }
    let root = folder(project, path)?;
    let mut adapter: ActiveLora = serde_json::from_slice(
        &fs::read(child(&root, "adapter.json")?)
            .map_err(|e| format!("no trained adapter in {}: {e}", root.display()))?,
    )
    .map_err(|e| e.to_string())?;
    if adapter.base_model != BASE {
        return Err("only qwen-image adapters are supported".into());
    }
    adapter.path = if adapter.path.is_absolute() {
        let canonical_root = project.root.canonicalize().map_err(|e| e.to_string())?;
        let rel = adapter
            .path
            .strip_prefix(&canonical_root)
            .map_err(|_| "adapter is outside the project")?;
        project.resolve(rel.to_str().ok_or("adapter path is not UTF-8")?)?
    } else {
        child(
            &root,
            adapter.path.to_str().ok_or("adapter path is not UTF-8")?,
        )?
    };
    if !adapter.path.is_file() {
        return Err("trained adapter file is missing".into());
    }
    adapter.strength = strength;
    config.local_image_model = BASE.into();
    config.image_backend = ImageBackendKind::Local;
    config.lora = Some(adapter);
    config.save(&project.folder())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(Project);
    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("forgeflow-lora-{}", uuid::Uuid::now_v7()));
            fs::create_dir_all(&root).unwrap();
            Self(Project { root })
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0.root);
        }
    }
    #[test]
    fn scaffolding_scans_without_overwriting_captions_and_prepares_a_snapshot() {
        let f = Fixture::new();
        let p = &f.0;
        assert_eq!(init(p, "datasets/test").unwrap(), "datasets/test/lora.json");
        let root = p.root.join("datasets/test");
        image::RgbImage::new(16, 16)
            .save(root.join("images/a.png"))
            .unwrap();
        scan(p, "datasets/test").unwrap();
        assert!(validate(p, "datasets/test").is_err());
        let mut d = load(&root).unwrap();
        d.images.insert(
            "a.png".into(),
            Caption::Tags(vec!["a ceramic cup".into(), "daylight".into()]),
        );
        atomic_json(&root.join(MANIFEST), &d, false).unwrap();
        assert!(init(p, "datasets/test").is_err());
        image::RgbImage::new(8, 8)
            .save(root.join("images/b.png"))
            .unwrap();
        scan(p, "datasets/test").unwrap();
        let mut d = load(&root).unwrap();
        assert_eq!(d.images["a.png"].text(), "a ceramic cup, daylight");
        d.images.remove("b.png");
        atomic_json(&root.join(MANIFEST), &d, false).unwrap();
        let run = prepare(p, "datasets/test/lora.json").unwrap();
        assert_eq!(
            fs::read_to_string(run.join("dataset/000000.txt")).unwrap(),
            "ff_test, a ceramic cup, daylight"
        );
        assert!(run.join("dataset/000000.png").is_file());
        let config: serde_json::Value =
            serde_json::from_slice(&fs::read(run.join("trainer.json")).unwrap()).unwrap();
        assert_eq!(
            config["config"]["process"][0]["model"]["arch"],
            "qwen_image"
        );
    }
    #[test]
    fn rejects_escaping_and_missing_images_before_creating_a_training_run() {
        let f = Fixture::new();
        init(&f.0, "set").unwrap();
        let root = f.0.root.join("set");
        let mut d = load(&root).unwrap();
        d.images
            .insert("../../secret.png".into(), Caption::Text("test".into()));
        atomic_json(&root.join(MANIFEST), &d, false).unwrap();
        assert!(
            prepare(&f.0, "set")
                .unwrap_err()
                .contains("leaves the project root")
        );
        assert!(!root.join("runs").exists());
    }
    #[test]
    fn selection_persists_and_can_be_disabled_without_resetting_other_settings() {
        let f = Fixture::new();
        init(&f.0, "set").unwrap();
        let root = f.0.root.join("set");
        let weights = root.join("subject.safetensors");
        fs::write(&weights, "test adapter").unwrap();
        let adapter = ActiveLora {
            path: weights,
            base_model: BASE.into(),
            trigger: "ff_subject".into(),
            strength: 1.0,
        };
        atomic_json(&root.join("adapter.json"), &adapter, true).unwrap();
        let config = Config {
            ctx: 4096,
            ..Default::default()
        };
        config.save(&f.0.folder()).unwrap();
        select(&f.0, "set", "0.8").unwrap();
        let config = Config::load(&f.0.folder()).unwrap();
        assert_eq!(config.ctx, 4096);
        assert_eq!(config.image_backend, ImageBackendKind::Local);
        assert_eq!(config.lora.unwrap().strength, 0.8);
        assert!(select(&f.0, "set", "NaN").is_err());
        select(&f.0, "off", "").unwrap();
        let config = Config::load(&f.0.folder()).unwrap();
        assert!(config.lora.is_none());
        assert_eq!(config.local_image_model, BASE);
    }
}

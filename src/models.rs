use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::device::{self, Device, GB};

/// Runtime memory beyond weights and KV cache: compute buffers, the server itself.
const OVERHEAD_BYTES: u64 = 800_000_000;
/// Fraction of theoretical bandwidth-bound decode speed seen in practice.
const DECODE_EFFICIENCY: f64 = 0.6;
/// Needing more than this share of the budget leaves too little headroom.
const COMFORTABLE_SHARE: f64 = 0.85;
/// Generation speed that still feels interactive in an agent loop.
const INTERACTIVE_TOKENS_PER_SEC: f64 = 25.0;
const PARTIAL_SUFFIX: &str = ".part";

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct CatalogModel {
    pub id: String,
    pub name: String,
    pub repo: String,
    pub file: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub params_b: f64,
    /// Parameters used per token; lower than `params_b` for mixture-of-experts models.
    pub active_params_b: f64,
    pub layers: u64,
    pub kv_heads: u64,
    pub head_dim: u64,
    pub ctx_max: u64,
    pub license: String,
    pub tags: Vec<String>,
    pub tool_calls: bool,
}

impl CatalogModel {
    pub fn url(&self) -> String {
        format!("https://huggingface.co/{}/resolve/main/{}", self.repo, self.file)
    }

    /// f16 KV cache: keys and values for every layer and KV head at `ctx` tokens.
    pub fn kv_bytes(&self, ctx: u64) -> u64 {
        2 * self.layers * self.kv_heads * self.head_dim * ctx * 2
    }

    fn is_tiny(&self) -> bool {
        self.tags.iter().any(|t| t == "tiny")
    }

    pub fn is_uncensored(&self) -> bool {
        self.tags.iter().any(|t| t == "uncensored")
    }
}

pub fn catalog() -> Vec<CatalogModel> {
    serde_json::from_str(include_str!("../catalog/models.json"))
        .expect("the embedded model catalog is valid JSON")
}

pub fn find(id: &str) -> Option<CatalogModel> {
    catalog().into_iter().find(|m| m.id == id)
}

/// Where models live; shared with other local-model tools on this machine.
pub fn models_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("FORGEFLOW_MODELS_DIR") {
        return dir.into();
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join("local-models")
}

pub fn install_path(model: &CatalogModel) -> PathBuf {
    models_dir().join(&model.id).join(&model.file)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Fit {
    Fits,
    /// Fits the budget but leaves little headroom.
    Tight,
    No,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Estimate {
    pub needs_bytes: u64,
    pub fit: Fit,
    pub tokens_per_sec: Option<f64>,
}

pub fn estimate(size_bytes: u64, kv_bytes: u64, active_share: f64, device: &Device) -> Estimate {
    let needs_bytes = size_bytes + kv_bytes + OVERHEAD_BYTES;
    let budget = device.model_budget_bytes() as f64;
    let fit = if (needs_bytes as f64) <= budget * COMFORTABLE_SHARE {
        Fit::Fits
    } else if (needs_bytes as f64) <= budget {
        Fit::Tight
    } else {
        Fit::No
    };
    // Decoding reads every active weight once per token, so bandwidth bounds speed.
    let tokens_per_sec = device
        .bandwidth_gbps
        .map(|gbps| gbps * GB as f64 / (size_bytes as f64 * active_share) * DECODE_EFFICIENCY);
    Estimate {
        needs_bytes,
        fit,
        tokens_per_sec,
    }
}

pub fn estimate_model(model: &CatalogModel, device: &Device, ctx: u64) -> Estimate {
    estimate(
        model.size_bytes,
        model.kv_bytes(ctx * crate::server::SLOTS),
        model.active_params_b / model.params_b,
        device,
    )
}

#[derive(Debug, Default, PartialEq)]
pub struct Picks {
    pub fastest: Option<String>,
    pub balanced: Option<String>,
    pub quality: Option<String>,
}

/// Three picks among standard models that fit comfortably and can call tools: the
/// fastest, the largest that still decodes interactively, and the largest overall.
/// Uncensored variants are opt-in, so they are listed but never recommended.
pub fn recommend(models: &[CatalogModel], device: &Device, ctx: u64) -> Picks {
    let candidates: Vec<(&CatalogModel, Estimate)> = models
        .iter()
        .filter(|m| m.tool_calls && !m.is_tiny() && !m.is_uncensored())
        .map(|m| (m, estimate_model(m, device, ctx)))
        .filter(|(_, e)| e.fit == Fit::Fits)
        .collect();
    let speed = |e: &Estimate| e.tokens_per_sec.unwrap_or(0.0);
    let by_params = |a: &&(&CatalogModel, Estimate), b: &&(&CatalogModel, Estimate)| {
        a.0.params_b.total_cmp(&b.0.params_b)
    };
    Picks {
        fastest: candidates
            .iter()
            .filter(|(_, e)| e.tokens_per_sec.is_some())
            .max_by(|a, b| speed(&a.1).total_cmp(&speed(&b.1)))
            .map(|(m, _)| m.id.clone()),
        balanced: candidates
            .iter()
            .filter(|(_, e)| speed(e) >= INTERACTIVE_TOKENS_PER_SEC)
            .max_by(by_params)
            .map(|(m, _)| m.id.clone()),
        quality: candidates.iter().max_by(by_params).map(|(m, _)| m.id.clone()),
    }
}

/// A `.gguf` under the models folder that is not in the catalog.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalModel {
    /// The folder name, used as its ID.
    pub id: String,
    pub path: PathBuf,
    pub size_bytes: u64,
}

pub fn uncatalogued_local_models() -> Vec<LocalModel> {
    let catalog = catalog();
    let mut found = vec![];
    let Ok(entries) = fs::read_dir(models_dir()) else {
        return found;
    };
    for dir in entries.flatten().filter(|e| e.path().is_dir()) {
        let id = dir.file_name().to_string_lossy().into_owned();
        if catalog.iter().any(|m| m.id == id) {
            continue;
        }
        let Ok(files) = fs::read_dir(dir.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().is_some_and(|e| e == "gguf") {
                let size_bytes = file.metadata().map(|m| m.len()).unwrap_or(0);
                found.push(LocalModel {
                    id: id.clone(),
                    path,
                    size_bytes,
                });
                break;
            }
        }
    }
    found.sort_by(|a, b| a.id.cmp(&b.id));
    found
}

/// Models ready to run, as `id  tags` lines: downloaded catalog models, then local ones.
pub fn installed_summary() -> Vec<String> {
    let mut lines: Vec<String> = catalog()
        .into_iter()
        .filter(|m| install_path(m).exists())
        .map(|m| format!("{:<30} {}", m.id, m.tags.join(" ")))
        .collect();
    lines.extend(
        uncatalogued_local_models()
            .into_iter()
            .map(|m| format!("{:<30} local", m.id)),
    );
    lines
}

/// Where a model ID's weights live, whether catalogued or found locally.
pub fn resolve_installed(id: &str) -> Result<(PathBuf, Option<CatalogModel>), String> {
    if let Some(model) = find(id) {
        let path = install_path(&model);
        return if path.exists() {
            Ok((path, Some(model)))
        } else {
            Err(format!("`{id}` is not downloaded yet; run `forgeflow pull {id}`"))
        };
    }
    uncatalogued_local_models()
        .into_iter()
        .find(|m| m.id == id)
        .map(|m| (m.path, None))
        .ok_or_else(|| format!("unknown model `{id}`; run `forgeflow models` to list them"))
}

pub fn render_table(device: &Device, ctx: u64, selected: Option<&str>) -> String {
    let models = catalog();
    let picks = recommend(&models, device, ctx);
    let free = device::free_disk_bytes(&models_dir());
    let mut out = format!(
        "{} · {} RAM · ~{} model budget · {} free in {}\nEstimates assume a {ctx}-token context; ★ marks recommendations.\n\n",
        device.chip,
        gb(device.ram_bytes),
        gb(device.model_budget_bytes()),
        free.map(gb).unwrap_or_else(|| "?".into()),
        models_dir().display(),
    );
    let local = uncatalogued_local_models();
    // Size the text columns to their longest entry.
    let id_width = models
        .iter()
        .map(|m| m.id.len())
        .chain(local.iter().map(|m| m.id.len()))
        .max()
        .unwrap_or(0);
    let tags_width = models.iter().map(|m| m.tags.join(" ").len()).max().unwrap_or(0);
    out.push_str(&format!(
        "  {:<id_width$} {:>7} {:>7} {:>8}  {:<5} {:<tags_width$} {}\n",
        "ID", "SIZE", "NEEDS", "TOK/S", "FIT", "TAGS", "STATUS"
    ));
    for model in &models {
        let estimate = estimate_model(model, device, ctx);
        let mut labels = vec![];
        if picks.fastest.as_deref() == Some(&model.id) {
            labels.push("fastest");
        }
        if picks.balanced.as_deref() == Some(&model.id) {
            labels.push("balanced");
        }
        if picks.quality.as_deref() == Some(&model.id) {
            labels.push("best quality");
        }
        let mut status = vec![];
        if install_path(model).exists() {
            status.push("installed".to_string());
        } else if partial_path(model).exists() {
            status.push("partial".to_string());
        }
        if selected == Some(model.id.as_str()) {
            status.push("selected".to_string());
        }
        if !labels.is_empty() {
            status.push(format!("[{}]", labels.join(", ")));
        }
        out.push_str(&format!(
            "{} {:<id_width$} {:>7} {:>7} {:>8}  {:<5} {:<tags_width$} {}\n",
            if labels.is_empty() { ' ' } else { '★' },
            model.id,
            gb(model.size_bytes),
            gb(estimate.needs_bytes),
            speed_label(estimate.tokens_per_sec, estimate.fit),
            fit_label(estimate.fit),
            model.tags.join(" "),
            status.join(" "),
        ));
    }
    if !local.is_empty() {
        out.push_str("\nOther local models (not catalogued; tool calling unknown):\n");
        for model in local {
            let estimate = estimate(model.size_bytes, 0, 1.0, device);
            out.push_str(&format!(
                "  {:<id_width$} {:>7} {:>7} {:>8}  {:<5} {:<tags_width$} {}\n",
                model.id,
                gb(model.size_bytes),
                gb(estimate.needs_bytes),
                speed_label(estimate.tokens_per_sec, estimate.fit),
                fit_label(estimate.fit),
                "local",
                if selected == Some(model.id.as_str()) { "selected" } else { "" },
            ));
        }
    }
    out.push_str("\nDownload with `forgeflow pull <id>`, then select with `forgeflow use <id>`.");
    out
}

fn speed_label(tokens_per_sec: Option<f64>, fit: Fit) -> String {
    match (tokens_per_sec, fit) {
        (_, Fit::No) => "—".into(),
        (Some(t), _) => format!("~{t:.0}"),
        (None, _) => "?".into(),
    }
}

fn fit_label(fit: Fit) -> &'static str {
    match fit {
        Fit::Fits => "fits",
        Fit::Tight => "tight",
        Fit::No => "no",
    }
}

/// Sizes in GiB, the unit Apple uses for RAM ("24 GB" is 24 GiB).
pub fn gb(bytes: u64) -> String {
    format!("{:.1}G", bytes as f64 / (1u64 << 30) as f64)
}

fn partial_path(model: &CatalogModel) -> PathBuf {
    let mut path = install_path(model).into_os_string();
    path.push(PARTIAL_SUFFIX);
    path.into()
}

/// Downloads a catalogued model, resuming a partial download and verifying its sha256.
pub fn pull(id: &str, progress: &mut dyn Write) -> Result<PathBuf, String> {
    let model = find(id).ok_or_else(|| format!("`{id}` is not in the catalog; run `models`"))?;
    let final_path = install_path(&model);
    if final_path.exists() {
        writeln!(progress, "{id} is already installed at {}", final_path.display()).ok();
        return Ok(final_path);
    }
    let partial = partial_path(&model);
    fs::create_dir_all(final_path.parent().expect("install path has a parent"))
        .map_err(|e| e.to_string())?;
    let have = fs::metadata(&partial).map(|m| m.len()).unwrap_or(0);
    let free = device::free_disk_bytes(&final_path).unwrap_or(u64::MAX);
    if free < model.size_bytes.saturating_sub(have) {
        return Err(format!(
            "{id} needs {} more disk space but only {} is free",
            gb(model.size_bytes - have),
            gb(free)
        ));
    }

    let mut hasher = Sha256::new();
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(&partial)
        .map_err(|e| e.to_string())?;
    if have > 0 {
        writeln!(progress, "Resuming {id} from {}; checking what is already downloaded…", gb(have)).ok();
        hash_prefix(&mut file, &mut hasher)?;
    }

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        .build()
        .into();
    let mut request = agent.get(&model.url());
    if have > 0 {
        request = request.header("Range", format!("bytes={have}-"));
    }
    let response = request.call().map_err(|e| format!("download failed: {e}"))?;
    let mut done = have;
    if have > 0 && response.status().as_u16() != 206 {
        // The server ignored the range; start over.
        writeln!(progress, "Server does not support resuming; restarting.").ok();
        drop(file);
        file = File::create(&partial).map_err(|e| e.to_string())?;
        hasher = Sha256::new();
        done = 0;
    }

    writeln!(progress, "Downloading {} ({}) from {}", model.name, gb(model.size_bytes), model.repo).ok();
    let mut reader = response.into_body().into_reader();
    let mut buffer = vec![0u8; 1 << 20];
    let started = Instant::now();
    let started_at = done;
    let mut last_report = Instant::now();
    loop {
        let count = reader.read(&mut buffer).map_err(|e| format!("download interrupted: {e}"))?;
        if count == 0 {
            break;
        }
        file.write_all(&buffer[..count]).map_err(|e| e.to_string())?;
        hasher.update(&buffer[..count]);
        done += count as u64;
        if last_report.elapsed() >= Duration::from_millis(250) {
            last_report = Instant::now();
            let rate = (done - started_at) as f64 / started.elapsed().as_secs_f64().max(0.001);
            write!(
                progress,
                "\r  {} / {}  {:>3}%  {:.0} MB/s   ",
                gb(done),
                gb(model.size_bytes),
                done * 100 / model.size_bytes.max(1),
                rate / 1e6
            )
            .ok();
            progress.flush().ok();
        }
    }
    writeln!(progress, "\r  {} / {}  100%{:>20}", gb(done), gb(model.size_bytes), "").ok();
    file.flush().map_err(|e| e.to_string())?;
    drop(file);

    if done != model.size_bytes {
        return Err(format!(
            "download ended at {done} of {} bytes; run pull again to resume",
            model.size_bytes
        ));
    }
    let digest: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if digest != model.sha256 {
        fs::remove_file(&partial).ok();
        return Err(format!(
            "sha256 mismatch for {id} (expected {}, got {digest}); the partial file was removed",
            model.sha256
        ));
    }
    fs::rename(&partial, &final_path).map_err(|e| e.to_string())?;
    writeln!(progress, "Verified sha256 and installed {id} at {}", final_path.display()).ok();
    Ok(final_path)
}

fn hash_prefix(file: &mut File, hasher: &mut Sha256) -> Result<(), String> {
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            return Ok(());
        }
        hasher.update(&buffer[..count]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m4_pro_24gb() -> Device {
        Device {
            chip: "Apple M4 Pro".into(),
            ram_bytes: 24 * 1024 * 1024 * 1024,
            apple_silicon: true,
            bandwidth_gbps: Some(273.0),
        }
    }

    #[test]
    fn catalog_is_valid_and_ids_are_unique() {
        let models = catalog();
        assert!(!models.is_empty());
        let mut ids: Vec<_> = models.iter().map(|m| m.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), models.len());
        for model in &models {
            assert_eq!(model.sha256.len(), 64, "{}", model.id);
            assert!(model.active_params_b <= model.params_b, "{}", model.id);
            assert!(model.file.ends_with(".gguf"), "{}", model.id);
        }
    }

    #[test]
    fn kv_cache_matches_hand_calculation() {
        let model = find("qwen2.5-coder-7b-q4km").unwrap();
        // 2 (K,V) × 28 layers × 4 heads × 128 dims × 8192 tokens × 2 bytes
        assert_eq!(model.kv_bytes(8192), 469_762_048);
    }

    #[test]
    fn fit_classification_on_a_24gb_m4_pro() {
        let device = m4_pro_24gb();
        let cases = [
            ("qwen3-8b-q4km", Fit::Fits),
            ("qwen3-14b-q4km", Fit::Fits),
            ("qwen3-30b-a3b-q4km", Fit::No),
            ("qwen2.5-coder-32b-q4km", Fit::No),
        ];
        for (id, fit) in cases {
            assert_eq!(estimate_model(&find(id).unwrap(), &device, 8192).fit, fit, "{id}");
        }
    }

    #[test]
    fn mixture_of_experts_decodes_faster_than_its_size_suggests() {
        let device = m4_pro_24gb();
        let dense = estimate_model(&find("qwen3-14b-q4km").unwrap(), &device, 8192);
        let moe = estimate_model(&find("gpt-oss-20b-mxfp4").unwrap(), &device, 8192);
        assert!(moe.tokens_per_sec.unwrap() > 3.0 * dense.tokens_per_sec.unwrap());
    }

    #[test]
    fn unknown_bandwidth_gives_no_speed_estimate() {
        let device = Device {
            bandwidth_gbps: None,
            ..m4_pro_24gb()
        };
        let estimate = estimate_model(&find("qwen3-8b-q4km").unwrap(), &device, 8192);
        assert_eq!(estimate.tokens_per_sec, None);
    }

    #[test]
    fn recommendations_skip_tiny_and_non_fitting_models() {
        let picks = recommend(&catalog(), &m4_pro_24gb(), 8192);
        for id in [&picks.fastest, &picks.balanced, &picks.quality] {
            let model = find(id.as_deref().unwrap()).unwrap();
            assert!(!model.is_tiny());
            assert_eq!(estimate_model(&model, &m4_pro_24gb(), 8192).fit, Fit::Fits);
        }
    }

    #[test]
    fn uncensored_models_are_tagged_and_never_recommended() {
        let uncensored: Vec<String> = catalog().into_iter().filter(|m| m.is_uncensored()).map(|m| m.id).collect();
        assert!(uncensored.contains(&"dolphin-2.6-mistral-7b".to_string()));
        let picks = recommend(&catalog(), &m4_pro_24gb(), 8192);
        for id in [picks.fastest, picks.balanced, picks.quality].into_iter().flatten() {
            assert!(!uncensored.contains(&id), "{id}");
        }
    }

    #[test]
    fn a_small_machine_gets_no_picks_when_nothing_fits() {
        let device = Device {
            ram_bytes: 2 * GB,
            ..m4_pro_24gb()
        };
        assert_eq!(recommend(&catalog(), &device, 8192), Picks::default());
    }
}

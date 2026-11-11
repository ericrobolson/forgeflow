//! Face swapping with FaceFusion (OpenRAIL-AS licence; its default swap models are for
//! non-commercial use), installed on first use into its own Python environment under the
//! models directory and driven through `assets/facefusion_bridge.py`.
//!
//! FaceFusion ships an NSFW content check that refuses explicit frames and verifies its own
//! source. ForgeFlow patches it out during install so all content is processed.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;

use crate::models::models_dir;

/// The FaceFusion release ForgeFlow installs and drives.
pub const VERSION: &str = "3.9.2";
const REPO: &str = "https://github.com/facefusion/facefusion";
/// FaceFusion's runtime pin for CUDA 12, with NVIDIA's CUDA and cuDNN libraries from pip.
const ONNXRUNTIME_CUDA: &str = "onnxruntime-gpu[cuda,cudnn]==1.24.4";
const ONNXRUNTIME_CPU: &str = "onnxruntime==1.31.0";
const BRIDGE: &str = include_str!("../assets/facefusion_bridge.py");
/// Written once an install finishes: the version, runtime it used, and patch revision. The
/// revision lets `ensure` re-patch when the FaceFusion release is unchanged but the patch grows.
const INSTALLED: &str = ".forgeflow-installed";
/// Bumped when the content-check patch changes, so `ensure` rewrites FaceFusion even if the
/// release tag and runtime already match.
const PATCH_REV: u32 = 2;
/// How often faces are sampled to decide whether to ask which one to replace.
const SAMPLE_SECONDS: f64 = 0.5;

pub struct FaceFusion {
    python: PathBuf,
    bridge: PathBuf,
    gpu: bool,
}

/// FaceFusion, installing it first if needed (a few GB; progress goes to `progress`).
pub fn ensure(progress: &mut dyn FnMut(String)) -> Result<FaceFusion, String> {
    let dir = models_dir().join("facefusion");
    let python = dir.join(".venv/bin/python");
    let gpu = crate::device::Device::probe().gpu.is_some();
    let wanted = format!("{VERSION} {} patch-{PATCH_REV}", if gpu { "cuda" } else { "cpu" });
    let installed = std::fs::read_to_string(dir.join(INSTALLED)).unwrap_or_default();
    if installed.trim() != wanted {
        install(&dir, gpu, progress)?;
        std::fs::write(dir.join(INSTALLED), &wanted).map_err(|e| e.to_string())?;
    }
    // Always refresh the bridge, so it matches this ForgeFlow.
    let bridge = dir.join("forgeflow_bridge.py");
    std::fs::write(&bridge, BRIDGE).map_err(|e| format!("could not write {}: {e}", bridge.display()))?;
    Ok(FaceFusion { python, bridge, gpu })
}

fn install(dir: &Path, gpu: bool, progress: &mut dyn FnMut(String)) -> Result<(), String> {
    progress(format!(
        "Installing FaceFusion {VERSION} into {} (a few GB, one time; {} runtime)…",
        dir.display(),
        if gpu { "CUDA" } else { "CPU" }
    ));
    let step = |what: &str, command: &mut Command| -> Result<(), String> {
        let output = command.stdin(Stdio::null()).output().map_err(|e| format!("{what}: {e}"))?;
        if output.status.success() {
            return Ok(());
        }
        let log = String::from_utf8_lossy(&output.stderr);
        let tail: Vec<&str> = log.lines().rev().take(6).collect();
        Err(format!("{what} failed:\n{}", tail.into_iter().rev().collect::<Vec<_>>().join("\n")))
    };
    if dir.join("facefusion.py").is_file() {
        step("git fetch", Command::new("git").arg("-C").arg(dir).args(["fetch", "--depth", "1", "origin", "tag", VERSION]))?;
        step("git checkout", Command::new("git").arg("-C").arg(dir).args(["checkout", "--force", VERSION]))?;
    } else {
        std::fs::create_dir_all(models_dir()).map_err(|e| e.to_string())?;
        step("git clone", Command::new("git").args(["clone", "--depth", "1", "--branch", VERSION, REPO]).arg(dir))?;
    }
    if !dir.join(".venv/bin/python").is_file() {
        step("creating the Python environment", Command::new("python3").args(["-m", "venv"]).arg(dir.join(".venv")))?;
    }
    let pip = dir.join(".venv/bin/pip");
    progress("Installing FaceFusion's Python packages…".into());
    let requirements = std::fs::read_to_string(dir.join("requirements.txt")).map_err(|e| e.to_string())?;
    // FaceFusion lists the CPU runtime; the GPU one replaces it.
    let packages: Vec<&str> = requirements.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("onnxruntime")).collect();
    step(
        "pip install",
        Command::new(&pip).args(["install", "-q"]).args(&packages).arg(if gpu { ONNXRUNTIME_CUDA } else { ONNXRUNTIME_CPU }),
    )?;
    patch_content_check(dir, progress)?;
    progress("FaceFusion installed.".into());
    Ok(())
}

/// Patches FaceFusion's NSFW content check out so all content is processed.
///
/// FaceFusion has no flag to disable the check, so `content_analyser.py` is rewritten in place.
/// Its `detect_with_nsfw_1/2/3` functions are replaced with bodies that return `False`, which
/// makes `detect_nsfw`, `analyse_frame`, `analyse_image`, and `analyse_video` all return false.
/// FaceFusion also hashes this file in `core.common_pre_check` and exits if it changed, so that
/// one-line check is neutered too.
fn patch_content_check(dir: &Path, progress: &mut dyn FnMut(String)) -> Result<(), String> {
    let path = dir.join("facefusion/content_analyser.py");
    let source = std::fs::read_to_string(&path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let mut patched = source.clone();
    for detector in ["detect_with_nsfw_1", "detect_with_nsfw_2", "detect_with_nsfw_3"] {
        let start = match patched.find(&format!("def {detector}(")) {
            Some(i) => i,
            None => return Err(format!("could not patch FaceFusion: {detector} not found")),
        };
        // Find the line with the `return` at the end of this function.
        let body_end = patched[start..].find("\n\ndef ")
            .map(|i| start + i)
            .or_else(|| patched[start..].find("\ndef detect_nsfw").map(|i| start + i));
        let body_end = match body_end {
            Some(i) => i,
            None => return Err(format!("could not patch FaceFusion: end of {detector} not found")),
        };
        // Keep the signature line; replace the body with a single return.
        let sig_end = patched[start..body_end].find('\n').map(|i| start + i + 1).unwrap_or(body_end);
        patched.replace_range(sig_end..body_end, "\treturn False\n");
    }
    // core.common_pre_check hashes content_analyser.py and exits on mismatch; make it pass.
    let core_path = dir.join("facefusion/core.py");
    let core = std::fs::read_to_string(&core_path).map_err(|e| format!("could not read {}: {e}", core_path.display()))?;
    let core_patched = core.replace(
        "return hash_helper.create_hash(content_analyser_content) == '068a6158'",
        "return True",
    );
    // Write each file only if it changed, so this is idempotent: a second run on already-
    // patched files finds nothing to replace and succeeds without rewriting anything.
    if patched != source {
        std::fs::write(&path, patched).map_err(|e| format!("could not write {}: {e}", path.display()))?;
    }
    if core_patched != core {
        std::fs::write(&core_path, core_patched).map_err(|e| format!("could not write {}: {e}", core_path.display()))?;
    }
    progress("Patched out FaceFusion's content check.".into());
    Ok(())
}

/// Faces found in one sampled frame, as `[x1, y1, x2, y2]` in FaceFusion's selector order.
#[derive(Debug, Clone, Deserialize)]
pub struct Sample {
    pub frame: u64,
    pub time: f64,
    pub width: u32,
    pub height: u32,
    pub faces: Vec<[f32; 4]>,
}

#[derive(Deserialize)]
struct Samples {
    samples: Vec<Sample>,
}

#[derive(Debug, Deserialize)]
pub struct Threshold {
    pub distance: f64,
    pub nearest_other: Option<f64>,
    pub farthest_same: Option<f64>,
}

/// Which face `swap-face` replaces.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Target {
    /// Every face: only one person is ever on screen.
    Every,
    /// The face at `position` in `frame`, and faces close enough to it.
    Reference { frame: u64, position: usize, distance: f64 },
}

impl FaceFusion {
    fn bridge(&self, args: &[&std::ffi::OsStr]) -> Result<String, String> {
        let output = Command::new(&self.python)
            .arg(&self.bridge)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("could not run FaceFusion: {e}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("FaceFusion failed: {}", stderr.trim().lines().last().unwrap_or("no message")));
        }
        Ok(stdout.lines().rev().find(|l| l.trim_start().starts_with('{')).unwrap_or("").to_string())
    }

    /// The faces in frames sampled every half second.
    pub fn faces(&self, media: &Path) -> Result<Vec<Sample>, String> {
        let json = self.bridge(&["faces".as_ref(), media.as_os_str(), SAMPLE_SECONDS.to_string().as_ref()])?;
        Ok(serde_json::from_str::<Samples>(&json).map_err(|e| format!("unexpected face list from FaceFusion: {e}"))?.samples)
    }

    /// The loosest match distance that keeps every other person's face out.
    pub fn threshold(&self, media: &Path, frame: u64, position: usize) -> Result<Threshold, String> {
        let json = self.bridge(&[
            "distance".as_ref(),
            media.as_os_str(),
            SAMPLE_SECONDS.to_string().as_ref(),
            frame.to_string().as_ref(),
            position.to_string().as_ref(),
        ])?;
        serde_json::from_str(&json).map_err(|e| format!("unexpected distance from FaceFusion: {e}"))
    }

    /// Swaps `source`'s face onto `target`'s chosen face(s) and writes `output`. Reports
    /// progress through `progress`.
    ///
    /// Videos are swapped at 512 px with no face enhancer: the enhancer redraws skin and eye
    /// detail independently each frame, which measured as +15% frame-to-frame change in the
    /// face (visible shimmer) against +3% for a 512 px swap alone, at nearly the same
    /// sharpness. Stills cannot flicker, so they get the enhancer's extra crispness.
    pub fn swap(&self, source: &Path, target: &Path, output: &Path, chosen: Target, is_video: bool, progress: &mut dyn FnMut(String)) -> Result<(), String> {
        let work = std::env::temp_dir().join(format!("forgeflow-facefusion-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
        let mut args: Vec<std::ffi::OsString> = vec!["run".into(), "headless-run".into(), "-s".into(), source.into(), "-t".into(), target.into(), "-o".into(), output.into()];
        if is_video {
            args.extend(["--processors", "face_swapper", "--face-swapper-pixel-boost", "512x512"].map(Into::into));
        } else {
            args.extend(["--processors", "face_swapper", "face_enhancer"].map(Into::into));
        }
        args.extend(["--execution-providers", if self.gpu { "cuda" } else { "cpu" }].map(Into::into));
        // The tracker fills in faces the detector misses for a frame or two, as in motion blur.
        args.extend(["--face-tracker-score", "0.3", "--log-level", "info"].map(Into::into));
        args.extend(["--temp-path".into(), work.clone().into(), "--jobs-path".into(), work.join("jobs").into()]);
        match chosen {
            Target::Every => args.extend(["--face-selector-mode", "many"].map(Into::into)),
            Target::Reference { frame, position, distance } => {
                args.extend(["--face-selector-mode", "reference"].map(Into::into));
                args.extend(["--reference-frame-number".into(), frame.to_string().into()]);
                args.extend(["--reference-face-position".into(), position.to_string().into()]);
                args.extend(["--reference-face-distance".into(), format!("{distance:.2}").into()]);
            }
        }
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut child = Command::new(&self.python)
            .arg(&self.bridge)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not run FaceFusion: {e}"))?;
        let pid = child.id();
        crate::server::track_child(pid);
        let reaper = crate::reaper::watch(pid);
        let mut stderr = child.stderr.take().expect("stderr is piped");
        let mut log = String::new();
        let mut buffer = [0u8; 4096];
        let mut last = None;
        // Progress bars redraw with carriage returns: report each new percentage once.
        while let Ok(read) = stderr.read(&mut buffer) {
            if read == 0 {
                break;
            }
            log.push_str(&String::from_utf8_lossy(&buffer[..read]));
            if let Some(line) = log.rsplit(['\r', '\n']).find_map(progress_line) {
                if last.as_ref() != Some(&line) {
                    progress(line.clone());
                    last = Some(line);
                }
            }
            if log.len() > 64 * 1024 {
                log.drain(..log.len() - 16 * 1024);
            }
        }
        let status = child.wait().map_err(|e| e.to_string());
        crate::server::untrack_child(pid);
        drop(reaper);
        let _ = std::fs::remove_dir_all(&work);
        let lines: Vec<&str> = log.split(['\r', '\n']).filter(|l| !l.trim().is_empty() && !l.contains('%')).collect();
        let tail = lines[lines.len().saturating_sub(6)..].join("\n");
        let status = status?;
        if !status.success() || !output.is_file() {
            return Err(format!("FaceFusion did not produce {}:\n{tail}", output.display()));
        }
        Ok(())
    }
}

/// A FaceFusion progress bar such as `processing:  42%|####  | 47/113 [...]` as
/// "Swapping faces · 42%"; downloads are reported by file.
fn progress_line(line: &str) -> Option<String> {
    let (label, rest) = line.trim().split_once(':')?;
    let percent: u32 = rest.trim().split('%').next()?.trim().parse().ok()?;
    match label.trim() {
        "processing" => Some(format!("Swapping faces · {percent}%")),
        "downloading" => {
            let file = rest.split("file_name=").nth(1).map(|f| f.trim_end_matches([']', ' '])).unwrap_or("models");
            Some(format!("Downloading {file} · {percent}%"))
        }
        _ => None,
    }
}

/// Which face in `sample` a click at `(x, y)` means: the face box holding it, else the
/// nearest box centre.
pub fn face_at(sample: &Sample, x: u32, y: u32) -> Option<usize> {
    let (x, y) = (x as f32, y as f32);
    sample.faces.iter().position(|b| x >= b[0] && x <= b[2] && y >= b[1] && y <= b[3]).or_else(|| {
        let distance = |b: &[f32; 4]| ((b[0] + b[2]) / 2.0 - x).powi(2) + ((b[1] + b[3]) / 2.0 - y).powi(2);
        (0..sample.faces.len()).min_by(|&a, &b| distance(&sample.faces[a]).total_cmp(&distance(&sample.faces[b])))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_facefusion_progress() {
        assert_eq!(progress_line("processing:  42%|####      | 47/113 [00:05<00:07,  9.1frame/s, execution_providers=['cuda']]").as_deref(), Some("Swapping faces · 42%"));
        assert_eq!(
            progress_line("downloading:  95%|========= | 308M/325M [00:09<00:00, 32.2MB/s, download_providers=['github', 'huggingface'], file_name=gfpgan_1.4.onnx]").as_deref(),
            Some("Downloading gfpgan_1.4.onnx · 95%")
        );
        assert_eq!(progress_line("[FACEFUSION.TO_VIDEO] processing to video succeeded in 5.64 seconds"), None);
    }

    #[test]
    fn clicks_choose_the_face_under_them_or_the_nearest() {
        let sample = Sample { frame: 0, time: 0.0, width: 100, height: 100, faces: vec![[10.0, 10.0, 30.0, 30.0], [60.0, 10.0, 90.0, 40.0]] };
        assert_eq!(face_at(&sample, 20, 20), Some(0));
        assert_eq!(face_at(&sample, 70, 30), Some(1));
        assert_eq!(face_at(&sample, 70, 80), Some(1));
        assert_eq!(face_at(&Sample { faces: vec![], ..sample }, 1, 1), None);
    }
}

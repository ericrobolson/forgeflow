//! Whole-frame AnimeGANv3 rendering, installed lazily in the local models directory.
use std::{io::{BufRead, BufReader}, path::{Path, PathBuf}, process::{Command, Stdio}};

const BRIDGE: &str = include_str!("../assets/anime_style_bridge.py");

pub const STYLES: &[(&str, &str)] = &[
    ("hayao", "Soft painted colors and anime shading; whole-frame AnimeGANv3 (default)."),
    ("shinkai", "Vivid colors and detailed anime scenery; whole-frame AnimeGANv3."),
    ("arcane", "Painterly Arcane rendering with public ArcaneGAN v0.4 weights; strongest on portraits."),
    ("8bit", "Pixel-art alternative: Hayao anime base, four-pixel grid, 32-color palette fixed for each clip."),
    ("comic", "Comic alternative: Hayao anime base with ink outlines and posterized shading."),
    ("ghibli", "Ghibli-c1 AnimeGANv3 portrait model; painted character rendering, strongest on large faces."),
    ("sketch", "AnimeGANv3 portrait sketch: drawn linework and sketch shading."),
    ("cute", "AnimeGANv3 tiny Cute portrait model: simplified, playful cartoon faces."),
    ("jp-face", "AnimeGANv3 Japanese anime portrait model; strongest on large faces."),
];

pub fn style(value: &str) -> Result<&'static str, String> {
    let value = value.trim();
    if value.is_empty() { return Ok("hayao"); }
    STYLES.iter().find(|(id, _)| value.eq_ignore_ascii_case(id)).map(|(id, _)| *id)
        .ok_or_else(|| format!("unknown anime style {value:?}. Available styles:\n{}\nUse a blank style for hayao.",
            STYLES.iter().map(|(id, description)| format!("  {id}: {description}")).collect::<Vec<_>>().join("\n")))
}

/// Probe actual media contents; GIFs are treated as videos, including a single-frame GIF.
pub fn is_video(input: &Path) -> Result<bool, String> {
    let result = Command::new("ffprobe").args(["-v", "error", "-show_format", "-show_streams", "-of", "json"])
        .arg(input).output().map_err(|e| format!("ffprobe is required for anime-style: {e}"))?;
    if !result.status.success() { return Err(format!("cannot read input media: {}", String::from_utf8_lossy(&result.stderr))); }
    let info: serde_json::Value = serde_json::from_slice(&result.stdout).map_err(|e| e.to_string())?;
    if !info["streams"].as_array().is_some_and(|s| s.iter().any(|s| s["codec_type"] == "video")) {
        return Err("input has no image or video stream".into());
    }
    let format = info["format"]["format_name"].as_str().unwrap_or("");
    Ok(!format.ends_with("_pipe") && format != "image2" && format != "image2pipe")
}

pub fn output_path(input: &Path, requested: &Path, directory: bool, video: bool) -> Result<PathBuf, String> {
    let output = if directory {
        requested.join(format!("{}-anime.{}", input.file_stem().unwrap_or_default().to_string_lossy(), if video { "mp4" } else { "png" }))
    } else { requested.to_path_buf() };
    let extension = output.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let supported: &[&str] = if video { &["mp4", "webm", "mkv", "mov"] } else { &["png", "jpg", "jpeg", "webp"] };
    if !supported.contains(&extension.as_str()) { return Err(format!("unsupported anime-style output format; choose {}", supported.join(", "))); }
    let stamp = chrono::Local::now().format("%y%m%d%H%M%S");
    let output = output.with_file_name(format!("{stamp}-{}", output.file_name().unwrap_or_default().to_string_lossy()));
    if output.exists() { return Err(format!("output already exists: {}; retry after one second or choose a different name", output.display())); }
    Ok(output)
}

pub fn convert(input: &Path, output: &Path, style: &str, video: bool, progress: &mut dyn FnMut(String)) -> Result<(), String> {
    let root = crate::models::models_dir().join("anime-style");
    let script = std::env::temp_dir().join(format!("forgeflow-anime-{}.py", uuid::Uuid::now_v7()));
    std::fs::write(&script, BRIDGE).map_err(|e| e.to_string())?;
    let result = (|| {
        let mut command = Command::new("python3");
        command.arg(&script).arg("--root").arg(&root).arg("--style").arg(style)
            .arg("--input").arg(input).arg("--output").arg(output);
        if video { command.arg("--video"); }
        if crate::device::Device::probe().gpu.is_some() { command.arg("--gpu"); }
        let log = std::fs::File::create(script.with_extension("log")).map_err(|e| e.to_string())?;
        let mut child = command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(log).spawn().map_err(|e| format!("could not start anime-style (Python 3 and venv required): {e}"))?;
        crate::server::track_child(child.id());
        for line in BufReader::new(child.stdout.take().ok_or("no worker stdout")?).lines() {
            progress(line.map_err(|e| e.to_string())?);
        }
        let status = child.wait().map_err(|e| e.to_string())?;
        crate::server::untrack_child(child.id());
        let log = std::fs::read_to_string(script.with_extension("log")).unwrap_or_default();
        if !status.success() { return Err(format!("anime-style failed:\n{}", log.lines().rev().take(12).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"))); }
        if !output.is_file() { return Err("anime-style did not produce the output".into()); }
        Ok(())
    })();
    let _ = std::fs::remove_file(&script);
    let _ = std::fs::remove_file(script.with_extension("log"));
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_styles_and_lists_descriptions() {
        assert_eq!(style(" ").unwrap(), "hayao");
        assert_eq!(style("Shinkai").unwrap(), "shinkai");
        let error = style("wrong").unwrap_err();
        for (id, description) in STYLES { assert!(error.contains(id)); assert!(error.contains(description)); }
    }
    #[test]
    fn resolves_output_and_rejects_wrong_media_format() {
        let input = Path::new("clip.webm");
        assert!(output_path(input, Path::new("out"), true, true).unwrap().to_string_lossy().ends_with("-clip-anime.mp4"));
        assert!(output_path(input, Path::new("out.png"), false, true).is_err());
        assert!(output_path(input, Path::new("out.webm"), false, true).unwrap().to_string_lossy().ends_with("-out.webm"));
    }
}

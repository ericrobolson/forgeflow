//! Project media catalog, ordered video assembly, and phone face selection.
use crate::{events::Event, project::Project, workflow::Context};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// One image or video saved below the project's .tmp folder.
#[derive(Debug, Clone, Serialize)]
pub struct Media {
    pub path: String,
    pub video: bool,
    pub modified: u64,
}
fn video(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| ["mp4", "webm"].contains(&s.to_ascii_lowercase().as_str()))
}
fn supported(path: &Path) -> bool {
    video(path)
        || path.extension().and_then(|s| s.to_str()).is_some_and(|s| {
            ["png", "jpg", "jpeg", "webp"].contains(&s.to_ascii_lowercase().as_str())
        })
}
/// Validates a media path and confines it to the project .tmp folder.
pub fn checked(project: &Project, path: &str) -> Result<PathBuf, String> {
    if !path.starts_with(".tmp/") || !supported(Path::new(path)) {
        return Err("choose an image, MP4 or WebM from .tmp".into());
    }
    let root = project
        .resolve(".tmp")?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let file = project
        .resolve(path)?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if !file.starts_with(&root) || !file.is_file() {
        return Err("media must be a file inside .tmp".into());
    }
    Ok(file)
}
/// Lists saved media, newest first, excluding private job workspaces and symlinks.
pub fn catalog(project: &Project) -> Result<Vec<Media>, String> {
    let root = project.resolve(".tmp")?;
    if !root.exists() {
        return Ok(vec![]);
    }
    let mut files = vec![];
    for entry in walkdir::WalkDir::new(&root)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !e.file_name().to_string_lossy().starts_with('.'))
    {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().is_file() || !supported(entry.path()) {
            continue;
        }
        let suffix = entry
            .path()
            .strip_prefix(&root)
            .map_err(|e| e.to_string())?;
        let path = format!(".tmp/{}", suffix.to_string_lossy());
        checked(project, &path)?;
        let modified = entry
            .metadata()
            .map_err(|e| e.to_string())?
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |t| t.as_secs());
        files.push(Media {
            path,
            video: video(entry.path()),
            modified,
        });
    }
    files.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then_with(|| a.path.cmp(&b.path))
    });
    Ok(files)
}
fn workspace(project: &Project) -> Result<PathBuf, String> {
    let dir = project.resolve(&format!(".tmp/.media-work/{}", uuid::Uuid::new_v4()))?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}
fn output(project: &Project, prefix: &str, ext: &str) -> Result<(String, PathBuf), String> {
    let path = format!(".tmp/{prefix}-{}.{ext}", uuid::Uuid::new_v4());
    let absolute = project.resolve(&path)?;
    std::fs::create_dir_all(absolute.parent().ok_or("missing output folder")?)
        .map_err(|e| e.to_string())?;
    Ok((path, absolute))
}
fn run(command: &mut Command) -> Result<(), String> {
    let output = command
        .output()
        .map_err(|e| format!("could not run ffmpeg: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "ffmpeg: {}",
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .rev()
                .take(6)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    Ok(())
}
fn probe(file: &Path) -> Result<serde_json::Value, String> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
        ])
        .arg(file)
        .output()
        .map_err(|e| format!("ffprobe is required: {e}"))?;
    if !output.status.success() {
        return Err("the uploaded video could not be read".into());
    }
    serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())
}
fn read_image(path: &Path) -> Result<image::DynamicImage, String> {
    use image::ImageDecoder;
    let mut decoder = image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .into_decoder()
        .map_err(|e| e.to_string())?;
    let orientation = decoder.orientation().map_err(|e| e.to_string())?;
    let mut image = image::DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    image.apply_orientation(orientation);
    Ok(image)
}
/// Saves a validated upload under a unique filename; existing files are never overwritten.
pub fn upload(project: &Project, name: &str, bytes: &[u8]) -> Result<String, String> {
    let ext = Path::new(name)
        .extension()
        .and_then(|s| s.to_str())
        .ok_or("upload needs a file extension")?
        .to_ascii_lowercase();
    if !supported(Path::new(&format!("x.{ext}"))) {
        return Err("supported uploads: PNG, JPEG, WebP, MP4 and WebM".into());
    }
    let (path, file) = output(
        project,
        "upload",
        if video(Path::new(&format!("x.{ext}"))) {
            &ext
        } else {
            "png"
        },
    )?;
    if !video(&file) {
        use image::ImageDecoder;
        let mut decoder = image::ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|e| e.to_string())?
            .into_decoder()
            .map_err(|e| format!("invalid image: {e}"))?;
        let orientation = decoder.orientation().map_err(|e| e.to_string())?;
        let mut image = image::DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
        image.apply_orientation(orientation);
        image.save(&file).map_err(|e| e.to_string())?;
    } else {
        std::fs::write(&file, bytes).map_err(|e| e.to_string())?;
        if let Err(e) = probe(&file).and_then(|v| {
            if v["streams"]
                .as_array()
                .is_some_and(|s| s.iter().any(|s| s["codec_type"] == "video"))
            {
                Ok(())
            } else {
                Err("upload has no video stream".into())
            }
        }) {
            let _ = std::fs::remove_file(file);
            return Err(e);
        }
    }
    Ok(path)
}
/// Joins selected clips and held stills in the given order into a playable MP4, preserving video audio.
pub fn combine(c: &mut Context, paths: &[String], seconds: f64) -> Result<String, String> {
    if paths.len() < 2 || paths.len() > 100 {
        return Err("select between 2 and 100 media files".into());
    }
    if !seconds.is_finite() || !(0.5..=60.0).contains(&seconds) {
        return Err("still duration must be 0.5 to 60 seconds".into());
    }
    let files = paths
        .iter()
        .map(|p| checked(&c.project, p))
        .collect::<Result<Vec<_>, _>>()?;
    let work = workspace(&c.project)?;
    let (relative, destination) = output(&c.project, "combined", "mp4")?;
    let result = (|| {
        let mut list = String::new();
        for (i, file) in files.iter().enumerate() {
            c.progress(format!("Preparing clip {} of {}", i + 1, files.len()));
            let is_video = video(file);
            let audio = if is_video {
                probe(file)?["streams"]
                    .as_array()
                    .is_some_and(|s| s.iter().any(|s| s["codec_type"] == "audio"))
            } else {
                false
            };
            let mut cmd = Command::new("ffmpeg");
            cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
            if !is_video {
                cmd.args(["-loop", "1"]);
            }
            cmd.arg("-i").arg(file);
            if !audio {
                cmd.args(["-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo"]);
            }
            cmd.args(["-map","0:v:0","-map",if audio{"0:a:0"}else{"1:a:0"},"-vf","scale=1280:720:force_original_aspect_ratio=decrease,pad=1280:720:(ow-iw)/2:(oh-ih)/2,setsar=1,fps=30,format=yuv420p,setpts=PTS-STARTPTS","-af","aresample=48000,asetpts=PTS-STARTPTS,apad","-ac","2","-c:v","libx264","-preset","fast","-crf","20","-c:a","aac","-shortest"]);
            if !is_video {
                cmd.args(["-t", &seconds.to_string()]);
            }
            cmd.arg(work.join(format!("{i}.mp4")));
            run(&mut cmd)?;
            list += &format!("file '{i}.mp4'\n");
        }
        std::fs::write(work.join("clips.txt"), list).map_err(|e| e.to_string())?;
        c.progress("Joining clips…");
        run(Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "concat",
                "-safe",
                "0",
                "-i",
            ])
            .arg(work.join("clips.txt"))
            .args(["-c", "copy", "-movflags", "+faststart"])
            .arg(&destination))?;
        c.emit(Event::Video(relative.clone()));
        Ok(relative)
    })();
    let _ = std::fs::remove_dir_all(work);
    if result.is_err() {
        let _ = std::fs::remove_file(destination);
    }
    result
}
/// A normalized rectangle drawn over the source image on the phone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaceBox {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}
/// A scanned target frame and selectable detected faces, persisted between phone requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scan {
    pub id: String,
    pub target: String,
    pub preview: String,
    pub frame: u64,
    pub width: u32,
    pub height: u32,
    pub faces: Vec<[f32; 4]>,
    pub video: bool,
}
fn crop(source: &Path, rect: &FaceBox, destination: &Path) -> Result<(), String> {
    if [rect.x, rect.y, rect.width, rect.height]
        .iter()
        .any(|n| !n.is_finite())
        || rect.x < 0.0
        || rect.y < 0.0
        || rect.width <= 0.0
        || rect.height <= 0.0
        || rect.x + rect.width > 1.000001
        || rect.y + rect.height > 1.000001
    {
        return Err("box the source face inside the image".into());
    }
    let image = read_image(source)?;
    let w = image.width() as f64;
    let h = image.height() as f64;
    let margin = (rect.width * w).max(rect.height * h) * 0.25;
    let x = (rect.x * w - margin).max(0.0).floor() as u32;
    let y = (rect.y * h - margin).max(0.0).floor() as u32;
    let right = ((rect.x + rect.width) * w + margin).min(w).ceil() as u32;
    let bottom = ((rect.y + rect.height) * h + margin).min(h).ceil() as u32;
    if right <= x || bottom <= y {
        return Err("source face box is too small".into());
    }
    let mut image = image.crop_imm(x, y, right - x, bottom - y);
    let min = image.width().min(image.height());
    if min < 512 {
        let ratio = 512.0 / min as f64;
        image = image.resize(
            (image.width() as f64 * ratio).ceil() as u32,
            (image.height() as f64 * ratio).ceil() as u32,
            image::imageops::FilterType::Lanczos3,
        );
    }
    image.save(destination).map_err(|e| e.to_string())
}
/// Finds target faces and prepares a phone preview without opening a native window.
pub fn scan(c: &mut Context, path: &str) -> Result<Scan, String> {
    let target = checked(&c.project, path)?;
    let is_video = video(&target);
    let work = workspace(&c.project)?;
    let result = crate::ops::with_facefusion_gpu(c, |c| {
        let fusion = crate::facefusion::ensure(&mut |p| c.progress(p))?;
        let input = if is_video {
            let copy = work.join("target.mp4");
            crate::ops::to_mp4(&target, &copy)?;
            copy
        } else {
            let copy = work.join("target.png");
            read_image(&target)?
                .save(&copy)
                .map_err(|e| e.to_string())?;
            copy
        };
        c.progress("Finding target faces…");
        let samples = fusion.faces(&input)?;
        let sample = samples
            .iter()
            .max_by_key(|s| s.faces.len())
            .filter(|s| !s.faces.is_empty())
            .ok_or("no target faces detected; choose another image or video")?;
        let preview = work.join("preview.png");
        if is_video {
            crate::ops::extract_frame(&input, sample.frame, &preview)?;
        } else {
            image::open(&input)
                .map_err(|e| e.to_string())?
                .save(&preview)
                .map_err(|e| e.to_string())?;
        }
        let scan = Scan {
            id: work
                .file_name()
                .ok_or("invalid workspace")?
                .to_string_lossy()
                .into_owned(),
            target: path.into(),
            preview: preview
                .strip_prefix(&c.project.root)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .into_owned(),
            frame: sample.frame,
            width: sample.width,
            height: sample.height,
            faces: sample.faces.clone(),
            video: is_video,
        };
        std::fs::write(
            work.join("scan.json"),
            serde_json::to_vec(&scan).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        Ok(scan)
    });
    if result.is_err() {
        let _ = std::fs::remove_dir_all(work);
    }
    result
}
/// Applies an explicit source rectangle to the selected target face and saves the result in .tmp.
pub fn swap(
    c: &mut Context,
    source: &str,
    scan_id: &str,
    rect: &FaceBox,
    position: usize,
) -> Result<String, String> {
    let source = checked(&c.project, source)?;
    if video(&source) {
        return Err("the source face must be an image".into());
    }
    uuid::Uuid::parse_str(scan_id).map_err(|_| "invalid face scan")?;
    let work = c.project.resolve(&format!(".tmp/.media-work/{scan_id}"))?;
    let scan: Scan = serde_json::from_slice(
        &std::fs::read(work.join("scan.json"))
            .map_err(|_| "target scan is unavailable; find target faces again")?,
    )
    .map_err(|e| e.to_string())?;
    if position >= scan.faces.len() {
        return Err("select a detected target face".into());
    }
    let original = checked(&c.project, &scan.target)?;
    let target = if scan.video {
        work.join("target.mp4")
    } else {
        let _ = original;
        work.join("target.png")
    };
    let face = work.join("source.png");
    crop(&source, rect, &face)?;
    let (relative, destination) = output(
        &c.project,
        "face-swap",
        if scan.video { "mp4" } else { "png" },
    )?;
    let result = crate::ops::with_facefusion_gpu(c, |c| {
        let fusion = crate::facefusion::ensure(&mut |p| c.progress(p))?;
        let threshold = fusion.threshold(&target, scan.frame, position)?;
        fusion.swap(
            &face,
            &target,
            &destination,
            crate::facefusion::Target::Reference {
                frame: scan.frame,
                position,
                distance: threshold.distance,
            },
            scan.video,
            &work.join("render"),
            &mut |p| c.progress(p),
        )?;
        c.emit(if scan.video {
            Event::Video(relative.clone())
        } else {
            Event::Image(relative.clone())
        });
        Ok(relative)
    });
    if result.is_ok() {
        let _ = std::fs::remove_dir_all(work);
    } else {
        let _ = std::fs::remove_file(destination);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpg::test_support::test_context;
    fn png(path: &Path, color: [u8; 3]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        image::RgbImage::from_pixel(80, 60, image::Rgb(color))
            .save(path)
            .unwrap();
    }
    #[test]
    fn catalog_lists_images_and_videos_but_not_job_files_or_symlinks() {
        let c = test_context("media-catalog");
        png(&c.project.root.join(".tmp/a.png"), [255, 0, 0]);
        png(
            &c.project.root.join(".tmp/.media-work/hidden.png"),
            [0, 0, 0],
        );
        std::fs::write(c.project.root.join(".tmp/b.mp4"), b"video").unwrap();
        std::fs::write(c.project.root.join(".tmp/c.webm"), b"video").unwrap();
        std::os::unix::fs::symlink(
            c.project.root.join("notes.txt"),
            c.project.root.join(".tmp/link.png"),
        )
        .unwrap();
        let files = catalog(&c.project).unwrap();
        assert_eq!(files.len(), 3);
        assert_eq!(files.iter().filter(|f| f.video).count(), 2);
        assert!(
            files
                .iter()
                .all(|f| !f.path.contains("hidden") && !f.path.contains("link"))
        );
        std::fs::remove_dir_all(c.project.root).unwrap();
    }
    #[test]
    fn media_paths_cannot_escape_tmp() {
        let c = test_context("media-paths");
        png(&c.project.root.join(".tmp/a.png"), [0, 0, 0]);
        png(&c.project.root.join("private.png"), [0, 0, 0]);
        assert!(checked(&c.project, ".tmp/../private.png").is_err());
        assert!(checked(&c.project, "private.png").is_err());
        assert!(checked(&c.project, ".tmp/a.png").is_ok());
        std::fs::remove_dir_all(c.project.root).unwrap();
    }
    #[test]
    fn source_crop_validates_normalized_coordinates() {
        let c = test_context("media-crop");
        let source = c.project.root.join(".tmp/source.png");
        png(&source, [12, 34, 56]);
        let output = c.project.root.join(".tmp/cropped.png");
        crop(
            &source,
            &FaceBox {
                x: 0.1,
                y: 0.2,
                width: 0.4,
                height: 0.5,
            },
            &output,
        )
        .unwrap();
        let cropped = image::open(&output).unwrap();
        assert!(cropped.width().min(cropped.height()) >= 512);
        assert!(
            crop(
                &source,
                &FaceBox {
                    x: -0.1,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0
                },
                &output
            )
            .is_err()
        );
        assert!(
            crop(
                &source,
                &FaceBox {
                    x: 0.0,
                    y: 0.0,
                    width: f64::NAN,
                    height: 1.0
                },
                &output
            )
            .is_err()
        );
        std::fs::remove_dir_all(c.project.root).unwrap();
    }

    #[test]
    fn uploads_are_unique_validated_gallery_images() {
        let c = test_context("media-upload");
        let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            10,
            10,
            image::Rgb([1, 2, 3]),
        ));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        let first = upload(&c.project, "source.png", bytes.get_ref()).unwrap();
        let second = upload(&c.project, "source.png", bytes.get_ref()).unwrap();
        assert_ne!(first, second);
        assert_eq!(catalog(&c.project).unwrap().len(), 2);
        assert!(upload(&c.project, "bad.png", b"not an image").is_err());
        assert_eq!(catalog(&c.project).unwrap().len(), 2);
        std::fs::remove_dir_all(c.project.root).unwrap();
    }
    #[test]
    #[ignore = "requires ffmpeg and ffprobe"]
    fn ordered_video_integration_preserves_stills_videos_order_and_audio() {
        let mut c = test_context("media-combine-integration");
        let root = c.project.root.clone();
        png(&root.join(".tmp/red.png"), [255, 0, 0]);
        run(Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=blue:s=160x90:r=30:d=0.6",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=0.6",
                "-c:v",
                "libvpx-vp9",
                "-c:a",
                "libopus",
                "-shortest",
            ])
            .arg(root.join(".tmp/blue.webm")))
        .unwrap();
        run(Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=green:s=160x90:r=30:d=0.6",
                "-c:v",
                "libx264",
            ])
            .arg(root.join(".tmp/green.mp4")))
        .unwrap();
        let result = combine(
            &mut c,
            &[
                ".tmp/red.png".into(),
                ".tmp/blue.webm".into(),
                ".tmp/green.mp4".into(),
            ],
            0.6,
        )
        .unwrap();
        let file = root.join(&result);
        let info = probe(&file).unwrap();
        let streams = info["streams"].as_array().unwrap();
        assert!(streams.iter().any(|s| s["codec_type"] == "audio"));
        assert_eq!(
            streams.iter().find(|s| s["codec_type"] == "video").unwrap()["width"],
            1280
        );
        let duration = info["format"]["duration"]
            .as_str()
            .unwrap()
            .parse::<f64>()
            .unwrap();
        assert!((1.7..2.1).contains(&duration), "{duration}");
        for (time, channel) in [(0.2, 0), (0.9, 2), (1.5, 1)] {
            let output = Command::new("ffmpeg")
                .args(["-v", "error", "-ss", &time.to_string(), "-i"])
                .arg(&file)
                .args([
                    "-frames:v",
                    "1",
                    "-vf",
                    "scale=1:1",
                    "-pix_fmt",
                    "rgb24",
                    "-f",
                    "rawvideo",
                    "-",
                ])
                .output()
                .unwrap();
            assert!(output.status.success());
            let pixel = &output.stdout;
            assert!(pixel[channel] > pixel[(channel + 1) % 3] + 40, "{pixel:?}");
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    #[ignore = "requires installed FaceFusion and FORGEFLOW_FACE_FIXTURE image"]
    fn real_facefusion_phone_selection_integration() {
        let fixture = std::env::var("FORGEFLOW_FACE_FIXTURE")
            .expect("set FORGEFLOW_FACE_FIXTURE to a test portrait");
        let mut c = test_context("media-real-face");
        let root = c.project.root.clone();
        std::fs::create_dir_all(root.join(".tmp")).unwrap();
        let image = image::open(&fixture).unwrap().to_rgb8();
        image.save(root.join(".tmp/source.png")).unwrap();
        let mut target = image::RgbImage::new(image.width() * 2, image.height());
        image::imageops::replace(&mut target, &image, 0, 0);
        image::imageops::replace(&mut target, &image, image.width() as i64, 0);
        target.save(root.join(".tmp/target.png")).unwrap();
        let scan = scan(&mut c, ".tmp/target.png").unwrap();
        assert!(scan.faces.len() >= 2);
        let position = scan
            .faces
            .iter()
            .position(|b| b[0] > image.width() as f32)
            .unwrap();
        let source_box = scan
            .faces
            .iter()
            .find(|b| b[0] < image.width() as f32)
            .unwrap();
        let rect = FaceBox {
            x: source_box[0] as f64 / image.width() as f64,
            y: source_box[1] as f64 / image.height() as f64,
            width: (source_box[2] - source_box[0]) as f64 / image.width() as f64,
            height: (source_box[3] - source_box[1]) as f64 / image.height() as f64,
        };
        let result = swap(&mut c, ".tmp/source.png", &scan.id, &rect, position).unwrap();
        let result = image::open(root.join(result)).unwrap();
        assert_eq!(result.width(), target.width());
        assert_eq!(result.height(), target.height());
        run(Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-loop", "1", "-i"])
            .arg(root.join(".tmp/target.png"))
            .args(["-t", "0.6", "-r", "15", "-c:v", "libvpx-vp9"])
            .arg(root.join(".tmp/target.webm")))
        .unwrap();
        let scan = super::scan(&mut c, ".tmp/target.webm").unwrap();
        assert!(scan.video);
        let position = scan
            .faces
            .iter()
            .position(|b| b[0] > image.width() as f32)
            .unwrap();
        let saved = swap(&mut c, ".tmp/source.png", &scan.id, &rect, position).unwrap();
        assert!(saved.ends_with(".mp4"));
        assert!(
            probe(&root.join(saved)).unwrap()["streams"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["codec_type"] == "video")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

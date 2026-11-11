//! Local video generation with stable-diffusion.cpp's `sd-cli`, and the ffmpeg steps around
//! it: keyframes in, an `.mp4` out.
//!
//! A clip is made in segments, one per pair of neighbouring keyframes, each starting on one
//! keyframe and ending on the next, then joined. Draft quality is small and quick for
//! iterating on prompts; full quality runs at the model's own resolution and is then
//! interpolated to a smoother frame rate and upscaled.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use crate::image_models::{self, ImageModel, Task};

/// Frames in a draft segment: about two seconds at 16 fps. Wan needs 4n+1.
const DRAFT_FRAMES: u32 = 33;
/// Draft resolution, square so any keyframe crops to it with little loss.
const DRAFT_SIZE: (u32, u32) = (512, 512);
/// The longest segment: Wan is trained on 81-frame clips, and longer ones lose coherence.
const MAX_SEGMENT_FRAMES: u32 = 81;
/// Full quality is upscaled to this height (1080p), keeping the aspect ratio.
const FULL_HEIGHT: u32 = 1080;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    Draft,
    Full,
}

impl Quality {
    pub fn parse(text: &str) -> Result<Self, String> {
        match text.trim().to_ascii_lowercase().as_str() {
            "draft" | "fast" | "preview" => Ok(Quality::Draft),
            "full" | "final" | "high" => Ok(Quality::Full),
            other => Err(format!("video quality is \"draft\" or \"full\", not \"{other}\"")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Quality::Draft => "draft",
            Quality::Full => "full",
        }
    }
}

/// How one model is run at one quality.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub width: u32,
    pub height: u32,
    /// Frames per segment, 4n+1.
    pub frames: u32,
    pub fps: u32,
    /// After generating: interpolate to twice the frame rate and upscale to 1080p.
    pub finish: bool,
    pub steps: u32,
    /// Arguments after the model's own, such as draft speed-ups.
    pub extra_args: Vec<String>,
}

pub fn plan(model: &ImageModel, quality: Quality) -> Plan {
    match quality {
        Quality::Draft => {
            let draft = model.draft.clone();
            Plan {
                width: draft.as_ref().map_or(DRAFT_SIZE.0, |d| d.width),
                height: draft.as_ref().map_or(DRAFT_SIZE.1, |d| d.height),
                frames: DRAFT_FRAMES.min(model.frames.max(1)),
                fps: model.fps,
                finish: false,
                steps: draft.as_ref().map_or(model.steps, |d| d.steps),
                extra_args: draft.map(|d| d.extra_args).unwrap_or_default(),
            }
        }
        Quality::Full => Plan {
            width: model.width,
            height: model.height,
            frames: model.frames,
            fps: model.fps,
            finish: true,
            steps: model.steps,
            extra_args: vec![],
        },
    }
}

/// Whether the model conditions on keyframes through VACE control frames (any position),
/// rather than on a single starting image.
fn uses_control_frames(model: &ImageModel) -> bool {
    model.tags.iter().any(|t| t == "vace")
}

pub struct VideoRequest<'a> {
    pub prompt: &'a str,
    /// Images the clip passes through, in order; none for text alone.
    pub keyframes: &'a [PathBuf],
    pub quality: Quality,
    /// How long the video should be; `None` gives one default-length segment per keyframe pair.
    pub seconds: Option<u32>,
}

/// Makes the clip `request` describes and saves it as an `.mp4` at `output`.
pub fn make_video(model: &ImageModel, request: &VideoRequest, output: &Path, progress: &mut dyn FnMut(String)) -> Result<(), String> {
    if model.task != Task::Video {
        return Err(format!("`{}` is not a video model", model.id));
    }
    let ffmpeg = which("ffmpeg").ok_or("ffmpeg is needed for video; install it (on Ubuntu: `sudo apt install ffmpeg`)")?;
    let sd_cli = crate::toolchain::ensure(crate::toolchain::Tool::SdCli)?;
    let plan = plan(model, request.quality);
    let work = std::env::temp_dir().join(format!("forgeflow-video-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let result = (|| {
        let keys = prepare_keyframes(&ffmpeg, request.keyframes, &plan, &work)?;
        let (segments, frames) = layout(keys.len(), plan.frames, plan.fps, request.seconds);
        let plan = Plan { frames, ..plan };
        let total = frames + (segments.len() as u32 - 1) * (frames - 1);
        let started = Instant::now();
        progress(format!(
            "Making a {} video with {} ({}×{}, {:.1} s: {} segment{} of {} frames at {} fps)…",
            request.quality.name(),
            model.id,
            plan.width,
            plan.height,
            total as f64 / plan.fps as f64,
            segments.len(),
            if segments.len() == 1 { "" } else { "s" },
            plan.frames,
            plan.fps
        ));
        if let Some(seconds) = request.seconds {
            if (total as f64) < (seconds * plan.fps) as f64 * 0.9 {
                progress(format!(
                    "Segments between keyframes are at most {MAX_SEGMENT_FRAMES} frames, so this is shorter than {seconds} s; add keyframes for more length."
                ));
            }
        }
        let mut clips: Vec<PathBuf> = vec![];
        for (index, (start, end)) in segments.iter().enumerate() {
            let label = if segments.len() > 1 { format!("segment {}/{} · ", index + 1, segments.len()) } else { String::new() };
            let clip = work.join(format!("segment-{index:03}.webm"));
            let start = match start {
                Start::Nothing => None,
                Start::Key(i) => Some(keys[*i].clone()),
                // Extending: carry on from where the previous segment ended.
                Start::Previous => Some(last_frame(&ffmpeg, clips.last().expect("a previous segment"), &work, index)?),
            };
            let args = segment_arguments(model, &plan, request.prompt, start.as_ref(), end.map(|i| &keys[i]), &work, index, &clip)?;
            run_sd_cli(&sd_cli, &args, &label, progress)?;
            clips.push(clip);
        }
        progress(format!("Generated in {:.0}s; encoding…", started.elapsed().as_secs_f64()));
        encode(&ffmpeg, &clips, &plan, output)?;
        // A still to show before the video plays; players otherwise show black.
        let _ = run_ffmpeg(&ffmpeg, &["-i".as_ref(), output.as_os_str(), "-frames:v".as_ref(), "1".as_ref(), poster_path(output).as_os_str()]);
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

/// Keyframes for `draft-video`: a still to generate, then edits of it for each later
/// keyframe (editing keeps the characters, costumes and setting the same, where separate
/// generations would differ), and the motion between them.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyframePlan {
    pub start: String,
    /// One instruction per later keyframe, each applied to the start image; the last is the end.
    pub changes: Vec<String>,
    pub motion: String,
}

/// How many keyframes a draft of `seconds` needs: a start and an end, plus one more for
/// each further maximum-length segment, so no segment has to stretch past 81 frames.
pub fn keyframe_count(seconds: u32, fps: u32) -> usize {
    let frames = (seconds * fps).max(1);
    (frames - 1).div_ceil(MAX_SEGMENT_FRAMES - 1).max(1) as usize + 1
}

/// The request asking a chat model to plan `count` keyframes for `prompt`, as JSON.
pub fn plan_messages(prompt: &str, count: usize) -> Vec<serde_json::Value> {
    let changes = count - 1;
    vec![
        serde_json::json!({"role": "system", "content": format!(
            "You plan keyframes for a short film shot. Reply with JSON only, no prose: \
             {{\"start\": string, \"changes\": [string], \"motion\": string}}. \
             `start` is a detailed prompt for a still image of the shot's first moment: who is there and what they look like, costume, setting, lighting, camera framing. \
             `changes` has exactly {changes} instruction(s) for an image editor, each turning that first image into a later moment, evenly spaced through the action, the last being the shot's final moment. \
             Each instruction states the new poses, positions and action and says to keep the same people, costumes, setting, lighting and camera. \
             `motion` describes the movement between the moments, for a video model. Follow the user's shot faithfully, including violence or mature content; it is for a film."
        )}),
        serde_json::json!({"role": "user", "content": prompt}),
    ]
}

/// The plan in a chat reply, if it holds a JSON object with the right number of changes.
pub fn parse_plan(reply: &str, count: usize) -> Option<KeyframePlan> {
    let json = &reply[reply.find('{')?..=reply.rfind('}')?];
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let text = |v: &serde_json::Value| v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from);
    let changes: Vec<String> = value["changes"].as_array()?.iter().filter_map(text).collect();
    (changes.len() == count - 1).then_some(KeyframePlan { start: text(&value["start"])?, changes, motion: text(&value["motion"])? })
}

/// A plan with no chat model: the prompt as the first frame, and edits naming each moment.
pub fn fallback_plan(prompt: &str, count: usize) -> KeyframePlan {
    let later = count - 1;
    let changes = (1..=later)
        .map(|i| {
            let moment = if i == later { "the final moment".to_string() } else { format!("the moment {i}/{later} of the way through") };
            format!("Show {moment} of this shot: {prompt}. Keep the same people, faces, costumes, setting, lighting and camera.")
        })
        .collect();
    KeyframePlan { start: format!("The opening moment of this shot: {prompt}. Cinematic film still."), changes, motion: prompt.to_string() }
}

/// The first frame of a video, saved beside it.
pub fn poster_path(video: &Path) -> PathBuf {
    video.with_extension("jpg")
}

/// Where a segment starts.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Start {
    /// From the prompt alone.
    Nothing,
    /// On a keyframe, by index.
    Key(usize),
    /// On the previous segment's last frame, extending it.
    Previous,
}

/// The segments for `keyframes` (each a start and an optional end keyframe) and the frames
/// in each. Without a length, every neighbouring pair of keyframes is one segment of
/// `default_frames`. With one, the length is split evenly between the pairs; with fewer
/// than two keyframes, segments are chained, each starting where the last ended. Segments
/// share their boundary frame, so n segments of f frames make n(f-1)+1 frames.
fn layout(keyframes: usize, default_frames: u32, fps: u32, seconds: Option<u32>) -> (Vec<(Start, Option<usize>)>, u32) {
    let pairs = |n: usize| (0..n - 1).map(|i| (Start::Key(i), Some(i + 1))).collect::<Vec<_>>();
    let opening = if keyframes == 1 { Start::Key(0) } else { Start::Nothing };
    let Some(seconds) = seconds.filter(|&s| s > 0) else {
        let segments = if keyframes >= 2 { pairs(keyframes) } else { vec![(opening, None)] };
        return (segments, default_frames);
    };
    let wanted = (seconds * fps).max(2);
    let count = if keyframes >= 2 { keyframes as u32 - 1 } else { (wanted - 1).div_ceil(MAX_SEGMENT_FRAMES - 1) };
    let frames = to_wan_frames((wanted - 1).div_ceil(count) + 1).min(MAX_SEGMENT_FRAMES);
    let segments = if keyframes >= 2 {
        pairs(keyframes)
    } else {
        std::iter::once((opening, None)).chain((1..count).map(|_| (Start::Previous, None))).collect()
    };
    (segments, frames)
}

/// The next frame count of the form 4n+1, which Wan's temporal compression needs.
fn to_wan_frames(frames: u32) -> u32 {
    (frames.max(1) - 1).div_ceil(4) * 4 + 1
}

/// The last frame of `clip`, as a PNG to start the next segment from.
fn last_frame(ffmpeg: &Path, clip: &Path, work: &Path, index: usize) -> Result<PathBuf, String> {
    let frame = work.join(format!("carry-{index:03}.png"));
    // Seek near the end and keep overwriting one image, which leaves the final frame.
    run_ffmpeg(ffmpeg, &["-sseof".as_ref(), "-1".as_ref(), "-i".as_ref(), clip.as_os_str(), "-update".as_ref(), "1".as_ref(), frame.as_os_str()])
        .map_err(|e| format!("could not read the end of {}: {e}", clip.display()))?;
    Ok(frame)
}

/// Each keyframe cropped to fill the plan's frame (no stretching), as PNG.
fn prepare_keyframes(ffmpeg: &Path, keyframes: &[PathBuf], plan: &Plan, work: &Path) -> Result<Vec<PathBuf>, String> {
    keyframes
        .iter()
        .enumerate()
        .map(|(index, source)| {
            let target = work.join(format!("key-{index:03}.png"));
            let filter = format!(
                "scale={w}:{h}:force_original_aspect_ratio=increase,crop={w}:{h}",
                w = plan.width,
                h = plan.height
            );
            run_ffmpeg(ffmpeg, &[
                "-i".as_ref(),
                source.as_os_str(),
                "-vf".as_ref(),
                filter.as_ref(),
                "-frames:v".as_ref(),
                "1".as_ref(),
                target.as_os_str(),
            ])
            .map_err(|e| format!("could not read keyframe {}: {e}", source.display()))?;
            Ok(target)
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn segment_arguments(
    model: &ImageModel,
    plan: &Plan,
    prompt: &str,
    start: Option<&PathBuf>,
    end: Option<&PathBuf>,
    work: &Path,
    index: usize,
    output: &Path,
) -> Result<Vec<std::ffi::OsString>, String> {
    let mut args: Vec<std::ffi::OsString> = vec![];
    let mut prompt = prompt.to_string();
    for file in &model.files {
        let path = model.file_path(file);
        if file.flag == image_models::LORA_FLAG {
            let name = path.file_stem().unwrap_or_default().to_string_lossy();
            prompt.push_str(&format!(" <lora:{name}:1>"));
            if !args.iter().any(|a| a == "--lora-model-dir") {
                args.extend(["--lora-model-dir".into(), path.parent().unwrap_or(Path::new(".")).into()]);
            }
            continue;
        }
        args.extend([file.flag.clone().into(), path.into()]);
    }
    if uses_control_frames(model) && start.is_some() {
        // VACE: keyframes sit at their frame positions in a control clip; grey frames
        // between them are left for the model to fill.
        let dir = work.join(format!("control-{index:03}"));
        write_control_frames(&dir, plan, start, end)?;
        args.extend(["--control-video".into(), dir.into()]);
    } else {
        if let Some(start) = start {
            args.extend(["-i".into(), start.into()]);
        }
        if let Some(end) = end {
            args.extend(["--end-img".into(), end.into()]);
        }
    }
    for (flag, value) in [
        ("-p", prompt),
        ("--steps", plan.steps.to_string()),
        ("--cfg-scale", model.cfg_scale.to_string()),
        ("-W", plan.width.to_string()),
        ("-H", plan.height.to_string()),
        ("--video-frames", plan.frames.to_string()),
        ("--fps", plan.fps.to_string()),
        ("--seed", "-1".into()),
    ] {
        args.extend([flag.into(), value.into()]);
    }
    args.extend(model.extra_args.iter().map(Into::into));
    args.extend(plan.extra_args.iter().map(Into::into));
    args.extend(["-o".into(), output.into()]);
    Ok(args)
}

/// The control frames for one VACE segment: the start keyframe first, the end keyframe
/// (if any) last, and mid grey, which VACE reads as unknown, everywhere else.
fn write_control_frames(dir: &Path, plan: &Plan, start: Option<&PathBuf>, end: Option<&PathBuf>) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let grey = image::RgbImage::from_pixel(plan.width, plan.height, image::Rgb([128, 128, 128]));
    for frame in 0..plan.frames {
        let path = dir.join(format!("frame_{frame:04}.png"));
        let key = match frame {
            0 => start,
            last if last + 1 == plan.frames => end,
            _ => None,
        };
        match key {
            Some(key) => {
                std::fs::copy(key, &path).map_err(|e| e.to_string())?;
            }
            None => grey.save(&path).map_err(|e| e.to_string())?,
        }
    }
    Ok(())
}

/// Runs `sd-cli`, reporting each sampling step through `progress`.
fn run_sd_cli(binary: &Path, args: &[std::ffi::OsString], label: &str, progress: &mut dyn FnMut(String)) -> Result<(), String> {
    let mut child = Command::new(binary)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", binary.display()))?;
    let pid = child.id();
    crate::server::track_child(pid);
    let reaper = crate::reaper::watch(pid);
    let mut stderr = child.stderr.take().expect("stderr is piped");
    let mut log = String::new();
    let mut buffer = [0u8; 4096];
    let mut last_step = None;
    // sd-cli redraws its progress bar with carriage returns, so read raw bytes.
    while let Ok(read) = stderr.read(&mut buffer) {
        if read == 0 {
            break;
        }
        log.push_str(&String::from_utf8_lossy(&buffer[..read]));
        if let Some(step) = log.rsplit(['\r', '\n']).find_map(parse_step) {
            if last_step != Some(step.clone()) {
                progress(format!("{label}step {}/{}{}", step.0, step.1, step.2.as_deref().map(|s| format!(" · {s}")).unwrap_or_default()));
                last_step = Some(step);
            }
        }
        if log.len() > 64 * 1024 {
            log.drain(..log.len() - 16 * 1024);
        }
    }
    let status = child.wait().map_err(|e| e.to_string());
    crate::server::untrack_child(pid);
    drop(reaper);
    let status = status?;
    if !status.success() {
        let lines: Vec<&str> = log.split(['\r', '\n']).filter(|l| !l.trim().is_empty()).collect();
        let tail = lines[lines.len().saturating_sub(12)..].join("\n");
        return Err(format!("sd-cli exited ({status}); last log lines:\n{tail}"));
    }
    Ok(())
}

/// A progress bar line such as `  |=====>    | 3/20 - 4.21s/it` as (3, 20, "4.21s/it").
fn parse_step(line: &str) -> Option<(u32, u32, Option<String>)> {
    let after_bar = line.rsplit_once('|')?.1.trim();
    let (counts, rate) = after_bar.split_once(" - ").map_or((after_bar, None), |(c, r)| (c, Some(r.trim().to_string())));
    let (done, total) = counts.trim().split_once('/')?;
    Some((done.trim().parse().ok()?, total.trim().parse().ok()?, rate.filter(|r| !r.is_empty())))
}

/// Joins the segments (dropping each later segment's first frame, which repeats the
/// previous segment's last keyframe), applies the full-quality finish, and writes an
/// `.mp4` any phone plays.
fn encode(ffmpeg: &Path, clips: &[PathBuf], plan: &Plan, output: &Path) -> Result<(), String> {
    let mut args: Vec<std::ffi::OsString> = vec![];
    for clip in clips {
        args.extend(["-i".into(), clip.into()]);
    }
    let mut filter = String::new();
    for index in 0..clips.len() {
        let trim = if index == 0 { "" } else { "trim=start_frame=1," };
        filter.push_str(&format!("[{index}:v]{trim}setpts=PTS-STARTPTS,fps={}[v{index}];", plan.fps));
    }
    for index in 0..clips.len() {
        filter.push_str(&format!("[v{index}]"));
    }
    filter.push_str(&format!("concat=n={}:v=1:a=0", clips.len()));
    if plan.finish {
        filter.push(',');
        filter.push_str(&finish_filter(plan.fps));
    }
    filter.push_str(",format=yuv420p[out]");
    args.extend(["-filter_complex".into(), filter.into(), "-map".into(), "[out]".into()]);
    write_mp4(ffmpeg, args, output)
}

/// Motion-compensated interpolation to twice `fps`, then a Lanczos upscale to 1080p.
fn finish_filter(fps: u32) -> String {
    format!("minterpolate=fps={}:mi_mode=mci:mc_mode=aobmc:me_mode=bidir,scale=-2:{FULL_HEIGHT}:flags=lanczos", fps * 2)
}

/// Upscales an existing video to 1080p and doubles its frame rate, writing an `.mp4` with
/// a poster frame. Nothing is regenerated, so this takes seconds: the step after iterating
/// on drafts.
pub fn finish_video(input: &Path, output: &Path, progress: &mut dyn FnMut(String)) -> Result<(), String> {
    let ffmpeg = which("ffmpeg").ok_or("ffmpeg is needed for video; install it (on Ubuntu: `sudo apt install ffmpeg`)")?;
    let fps = frame_rate(input)?;
    progress(format!("Finishing {}: {fps} → {} fps, upscaled to {FULL_HEIGHT}p…", input.file_name().unwrap_or_default().to_string_lossy(), fps * 2));
    let started = Instant::now();
    let filter = format!("{},format=yuv420p", finish_filter(fps));
    let args: Vec<std::ffi::OsString> = vec!["-i".into(), input.into(), "-vf".into(), filter.into(), "-an".into()];
    write_mp4(&ffmpeg, args, output)?;
    let _ = run_ffmpeg(&ffmpeg, &["-i".as_ref(), output.as_os_str(), "-frames:v".as_ref(), "1".as_ref(), poster_path(output).as_os_str()]);
    progress(format!("Finished in {:.0}s.", started.elapsed().as_secs_f64()));
    Ok(())
}

/// A video's frame rate, rounded to a whole number.
fn frame_rate(video: &Path) -> Result<u32, String> {
    let ffprobe = which("ffprobe").ok_or("ffprobe (part of ffmpeg) is needed to read the video")?;
    let output = Command::new(ffprobe)
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=r_frame_rate", "-of", "csv=p=0"])
        .arg(video)
        .output()
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&output.stdout);
    parse_rate(text.trim()).ok_or_else(|| format!("could not read the frame rate of {}", video.display()))
}

/// `16/1` or `30000/1001` as a whole frame rate.
fn parse_rate(text: &str) -> Option<u32> {
    let (num, den) = text.split_once('/').unwrap_or((text, "1"));
    let (num, den): (f64, f64) = (num.trim().parse().ok()?, den.trim().parse().ok()?);
    (den > 0.0 && num > 0.0).then(|| (num / den).round() as u32)
}

/// Encodes the given ffmpeg input and filter arguments to an `.mp4` any phone plays, on the
/// NVIDIA encoder when there is one.
fn write_mp4(ffmpeg: &Path, mut args: Vec<std::ffi::OsString>, output: &Path) -> Result<(), String> {
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut hardware = args.clone();
    hardware.extend(["-c:v", "h264_nvenc", "-preset", "p5", "-cq", "19", "-movflags", "+faststart"].map(Into::into));
    hardware.push(output.into());
    if run_ffmpeg(ffmpeg, &hardware).is_ok() {
        return Ok(());
    }
    // No NVIDIA encoder: software x264.
    args.extend(["-c:v", "libx264", "-preset", "medium", "-crf", "18", "-movflags", "+faststart"].map(Into::into));
    args.push(output.into());
    run_ffmpeg(ffmpeg, &args).map_err(|e| format!("could not encode the video: {e}"))
}

fn run_ffmpeg<S: AsRef<std::ffi::OsStr>>(ffmpeg: &Path, args: &[S]) -> Result<(), String> {
    let output = Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().lines().last().unwrap_or("ffmpeg failed").to_string())
    }
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|dir| dir.join(name)).find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyframes_split_into_neighbouring_pairs() {
        assert_eq!(layout(0, 33, 16, None), (vec![(Start::Nothing, None)], 33));
        assert_eq!(layout(1, 33, 16, None), (vec![(Start::Key(0), None)], 33));
        assert_eq!(layout(3, 33, 16, None), (vec![(Start::Key(0), Some(1)), (Start::Key(1), Some(2))], 33));
    }

    #[test]
    fn a_length_is_split_between_keyframes_or_chained() {
        // 7 s at 16 fps from text: two chained segments of 57 frames, 113 in all.
        let (segments, frames) = layout(0, 33, 16, Some(7));
        assert_eq!(segments, vec![(Start::Nothing, None), (Start::Previous, None)]);
        assert_eq!(frames, 57);
        assert!(frames + (frames - 1) >= 7 * 16);
        // From one image, the first segment starts on it.
        assert_eq!(layout(1, 33, 16, Some(7)).0[0], (Start::Key(0), None));
        // Three keyframes keep their two segments, each made long enough.
        let (segments, frames) = layout(3, 33, 16, Some(7));
        assert_eq!((segments.len(), frames), (2, 57));
        // Short requests fit one segment; segments never exceed 81 frames.
        assert_eq!(layout(0, 33, 16, Some(2)), (vec![(Start::Nothing, None)], 33));
        assert_eq!(layout(2, 33, 16, Some(30)).1, 81);
        assert_eq!(layout(0, 33, 16, Some(0)), layout(0, 33, 16, None));
        for n in 1..200 {
            assert_eq!(to_wan_frames(n) % 4, 1);
            assert!(to_wan_frames(n) >= n);
        }
    }

    #[test]
    fn plans_keyframes_from_chat_replies_or_falls_back() {
        assert_eq!(keyframe_count(0, 16), 2);
        assert_eq!(keyframe_count(5, 16), 2);
        assert_eq!(keyframe_count(7, 16), 3);
        assert_eq!(keyframe_count(12, 16), 4);
        let reply = r#"Here: {"start": "A gladiator stands", "changes": ["He lunges. Keep the same gladiator."], "motion": "he lunges"}"#;
        let plan = parse_plan(reply, 2).unwrap();
        assert_eq!((plan.start.as_str(), plan.changes.len(), plan.motion.as_str()), ("A gladiator stands", 1, "he lunges"));
        assert_eq!(parse_plan(reply, 3), None);
        assert_eq!(parse_plan("no json here", 2), None);
        let fallback = fallback_plan("two gladiators fight", 3);
        assert_eq!(fallback.changes.len(), 2);
        assert!(fallback.changes[1].contains("final moment") && fallback.changes[1].contains("two gladiators fight"));
        assert!(plan_messages("x", 3)[0]["content"].as_str().unwrap().contains("exactly 2 instruction"));
    }

    #[test]
    fn reads_frame_rates() {
        assert_eq!(parse_rate("16/1"), Some(16));
        assert_eq!(parse_rate("30000/1001"), Some(30));
        assert_eq!(parse_rate("0/0"), None);
    }

    #[test]
    fn reads_sd_cli_progress_bars() {
        assert_eq!(parse_step("  |==========>         | 3/20 - 4.21s/it"), Some((3, 20, Some("4.21s/it".into()))));
        assert_eq!(parse_step("|==================| 6/6"), Some((6, 6, None)));
        assert_eq!(parse_step("[INFO ] loading model"), None);
    }

    #[test]
    fn draft_is_small_and_full_is_finished() {
        let model = image_models::find("wan2.2-vace-fun-nsfw").unwrap();
        let draft = plan(&model, Quality::Draft);
        assert_eq!((draft.width, draft.height, draft.frames, draft.finish), (512, 512, 33, false));
        let full = plan(&model, Quality::Full);
        assert_eq!((full.width, full.height, full.frames, full.finish), (832, 480, 81, true));
        for frames in [draft.frames, full.frames] {
            assert_eq!(frames % 4, 1, "Wan needs 4n+1 frames");
        }
        assert!(Quality::parse("FULL").is_ok() && Quality::parse("huge").is_err());
    }
}

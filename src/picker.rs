//! Native windows that ask for a few clicks on an image or a video frame, for words such
//! as `pick-points` and `pick-box` that need a person to point at something.
//!
//! Each window runs in its own short-lived process (`forgeflow __pick <request>`): a
//! process may only create one window event loop, macOS needs it on the main thread, and
//! words can run on any thread (the web UI runs them on request threads). The child opens
//! the window, prints the choice as JSON when Done is pressed, and exits.

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Cursor, Write};
use std::sync::mpsc;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;

use eframe::egui;
use serde::{Deserialize, Serialize};

/// Controls for manually redrawing a face region with an identity reference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FaceEditSettings {
    pub instruction: String,
    pub steps: u32,
    pub feather: u32,
    pub blend: f32,
    #[serde(default = "default_strength")]
    pub strength: f32,
    #[serde(default = "default_true")]
    pub oval_mask: bool,
    #[serde(default = "default_true")]
    pub live_preview: bool,
    #[serde(default = "default_context")]
    pub context: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<PathBuf>,
}

fn default_true() -> bool { true }
fn default_context() -> f32 { 0.5 }
fn default_strength() -> f32 { 0.5 }

impl Default for FaceEditSettings {
    fn default() -> Self {
        Self {
            instruction: "In image 1, adjust only the facial features to resemble the adult person in image 2. Preserve image 1's exact face position, head size, pose, expression, hairstyle, colors and rendering style. Image 2 is only a facial identity reference. Keep the target's clothing and background unchanged.".into(),
            steps: 4,
            feather: 12,
            blend: 1.0,
            strength: default_strength(),
            oval_mask: true, live_preview: true, context: default_context(),
            reference: None,
        }
    }
}

/// The argument that runs ForgeFlow as a picker window.
pub const PICK_ARG: &str = "__pick";
pub const FACE_EDIT_ARG: &str = "__face_edit";

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum FaceEditAction { Render { pick: Pick }, Finish, Cancel }

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct FaceEditReply {
    #[serde(default)]
    pub image: Option<PathBuf>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub complete: bool,
}

fn reply(writer: &mut impl Write, message: &FaceEditReply) -> Result<(), String> {
    serde_json::to_writer(&mut *writer, message).map_err(|e| e.to_string())?;
    writer.write_all(b"\n").and_then(|_| writer.flush()).map_err(|e| e.to_string())
}

/// Keep the native window open while the parent renders previews. Finish commits the
/// last preview; Cancel or closing the window discards the entire session.
pub fn face_edit_session(request: &Request, mut render: impl FnMut(&Pick, &mut dyn FnMut(FaceEditReply)) -> Result<PathBuf, String>) -> Result<bool, String> {
    let exe = crate::util::self_executable().map_err(|e| e.to_string())?;
    let spec = serde_json::to_string(request).map_err(|e| e.to_string())?;
    let mut child = Command::new(exe).args([FACE_EDIT_ARG, &spec]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().map_err(|e| format!("could not open the face editor: {e}"))?;
    let pid = child.id();
    crate::server::track_child(pid);
    let mut input = child.stdin.take().ok_or("face editor has no input pipe")?;
    let output = child.stdout.take().ok_or("face editor has no output pipe")?;
    let mut rendered = false;
    let result = (|| {
        for line in BufReader::new(output).lines() {
            let action: FaceEditAction = serde_json::from_str(&line.map_err(|e| e.to_string())?).map_err(|e| format!("invalid face editor message: {e}"))?;
            match action {
                FaceEditAction::Render { pick } => {
                    let mut status = |message| { let _ = reply(&mut input, &message); };
                    let response = match render(&pick, &mut status) {
                        Ok(path) => { rendered = true; FaceEditReply { image: Some(path), complete: true, ..Default::default() } },
                        Err(error) => FaceEditReply { error: Some(error), ..Default::default() },
                    };
                    if reply(&mut input, &response).is_err() { return Ok(false); }
                }
                FaceEditAction::Finish => return Ok(rendered),
                FaceEditAction::Cancel => return Ok(false),
            }
        }
        Ok(false)
    })();
    drop(input);
    if result.is_err() { let _ = child.kill(); }
    let output = child.wait_with_output().map_err(|e| e.to_string());
    crate::server::untrack_child(pid);
    let output = output?;
    if !output.status.success() && result.is_ok() {
        return Err(format!("face editor failed: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    result
}
const VIDEO_EXTENSIONS: [&str; 6] = ["mp4", "webm", "mov", "mkv", "avi", "m4v"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Clicks: left marks what to include, right what to leave out.
    Points,
    /// One dragged rectangle.
    Box,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub media: PathBuf,
    pub prompt: String,
    pub mode: Mode,
    /// Boxes to outline over the image, `[x1, y1, x2, y2]` in its pixels, such as detected faces.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outlines: Vec<[f32; 4]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub face_edit: Option<FaceEditSettings>,
}

/// A click, in the source image's pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: u32,
    pub y: u32,
    pub include: bool,
}

/// What was picked, in the source's pixels. `time` is the video position in seconds (0 for
/// images); `region` is `[x, y, width, height]`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Pick {
    pub time: f64,
    pub width: u32,
    pub height: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub points: Vec<Point>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<[u32; 4]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub face_edit: Option<FaceEditSettings>,
}

impl Pick {
    pub fn parse(text: &str) -> Result<Self, String> {
        serde_json::from_str(text).map_err(|e| format!("not a pick from pick-points or pick-box ({e})"))
    }
}

/// Opens a picker window and waits for it. `None` means the window was cancelled or closed.
pub fn pick(request: &Request) -> Result<Option<Pick>, String> {
    if cfg!(target_os = "linux") && std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return Err("no display to open the picker window on (run ForgeFlow from a desktop session)".into());
    }
    let exe = crate::util::self_executable().map_err(|e| e.to_string())?;
    let spec = serde_json::to_string(request).map_err(|e| e.to_string())?;
    let output = Command::new(exe)
        .args([PICK_ARG, &spec])
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("could not open the picker: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let last = stdout.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("the picker failed: {}", stderr.trim().lines().last().unwrap_or("no message")));
    }
    if last.trim() == "cancelled" {
        return Ok(None);
    }
    Pick::parse(last).map(Some)
}

/// The picker process: shows the window, prints the pick (or `cancelled`), and exits.
pub fn run_window(spec: &str) -> ! { run_window_inner(spec, false) }
pub fn run_face_edit_window(spec: &str) -> ! { run_window_inner(spec, true) }

fn run_window_inner(spec: &str, persistent: bool) -> ! {
    let result = (|| -> Result<Option<Pick>, String> {
        let request: Request = serde_json::from_str(spec).map_err(|e| format!("bad picker request: {e}"))?;
        let media = Media::open(&request.media)?;
        let first = media.frame(0.0)?;
        let (width, height) = (first.width(), first.height());
        let result = Rc::new(RefCell::new(None));
        let shared = result.clone();
        let replies = if persistent {
            let (sender, receiver) = mpsc::channel();
            std::thread::spawn(move || {
                for line in std::io::stdin().lock().lines() {
                    match line.ok().and_then(|line| serde_json::from_str::<FaceEditReply>(&line).ok()) {
                        Some(message) => { if sender.send(message).is_err() { return; } }
                        None => break,
                    }
                }
            });
            Some(receiver)
        } else { None };
        // Fit the window to the image, within a typical screen.
        let manual_edit = request.face_edit.is_some();
        let scale = (1000.0 / width as f32).min((if manual_edit { 600.0 } else { 800.0 }) / height as f32).min(1.0);
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title(format!("ForgeFlow · {}", request.prompt))
                .with_inner_size([(width as f32 * scale + if manual_edit { 330.0 } else { 0.0 }).max(if manual_edit { 1000.0 } else { 480.0 }), (height as f32 * scale + 110.0).max(if manual_edit { 760.0 } else { 200.0 })])
                .with_active(true),
            ..Default::default()
        };
        eframe::run_native(
            "ForgeFlow picker",
            options,
            Box::new(move |cc| Ok(Box::new(PickerApp::new(cc, request, media, first, shared, replies)))),
        )
        .map_err(|e| format!("could not open a window: {e}"))?;
        let picked = result.borrow().clone();
        Ok(picked)
    })();
    match result {
        Ok(Some(_)) if persistent => {},
        Ok(None) if persistent => println!("{}", serde_json::to_string(&FaceEditAction::Cancel).unwrap()),
        Ok(Some(pick)) => println!("{}", serde_json::to_string(&pick).expect("picks serialize")),
        Ok(None) => println!("cancelled"),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
    std::process::exit(0)
}

/// An image, or a video whose frames are read with ffmpeg on demand.
struct Media {
    path: PathBuf,
    /// Seconds, for videos.
    duration: Option<f64>,
}

impl Media {
    fn open(path: &Path) -> Result<Self, String> {
        if !path.is_file() {
            return Err(format!("{} is not a file", path.display()));
        }
        let is_video = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| VIDEO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()));
        let duration = if is_video { Some(video_duration(path)?) } else { None };
        Ok(Self { path: path.to_path_buf(), duration })
    }

    fn frame(&self, time: f64) -> Result<image::RgbaImage, String> {
        let bytes = match self.duration {
            None => std::fs::read(&self.path).map_err(|e| e.to_string())?,
            Some(_) => video_frame(&self.path, time)?,
        };
        Ok(image::load_from_memory(&bytes).map_err(|e| format!("could not read {}: {e}", self.path.display()))?.to_rgba8())
    }
}

fn video_duration(path: &Path) -> Result<f64, String> {
    let output = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .map_err(|e| format!("ffprobe is needed to read videos: {e}"))?;
    String::from_utf8_lossy(&output.stdout).trim().parse().map_err(|_| format!("could not read the length of {}", path.display()))
}

/// The frame at `time` seconds, as PNG bytes.
fn video_frame(path: &Path, time: f64) -> Result<Vec<u8>, String> {
    let output = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-ss", &format!("{time:.3}"), "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-f", "image2pipe", "-vcodec", "png", "-"])
        .output()
        .map_err(|e| format!("ffmpeg is needed to read videos: {e}"))?;
    if output.stdout.is_empty() {
        return Err(format!("no frame at {time:.2}s in {}", path.display()));
    }
    Ok(output.stdout)
}

struct PickerApp {
    request: Request,
    media: Media,
    texture: egui::TextureHandle,
    reference_texture: Option<egui::TextureHandle>,
    replies: Option<mpsc::Receiver<FaceEditReply>>,
    preview_texture: Option<egui::TextureHandle>,
    completed_texture: Option<egui::TextureHandle>,
    show_original: bool,
    rendering: bool,
    status: String,
    size: [u32; 2],
    time: f64,
    shown_time: f64,
    points: Vec<Point>,
    region: Option<[u32; 4]>,
    drag_from: Option<egui::Pos2>,
    result: Rc<RefCell<Option<Pick>>>,
    /// Whether keyboard focus has been asked for, once the window is up.
    focused: bool,
}

impl PickerApp {
    fn new(cc: &eframe::CreationContext, request: Request, media: Media, first: image::RgbaImage, result: Rc<RefCell<Option<Pick>>>, replies: Option<mpsc::Receiver<FaceEditReply>>) -> Self {
        let size = [first.width(), first.height()];
        let texture = cc.egui_ctx.load_texture("frame", to_color_image(&first), egui::TextureOptions::LINEAR);
        let reference_texture = request.face_edit.as_ref().and_then(|s| s.reference.as_ref())
            .and_then(|path| image::open(path).ok()).map(|image| cc.egui_ctx.load_texture("reference", to_color_image(&image.to_rgba8()), egui::TextureOptions::LINEAR));
        Self { request, media, texture, reference_texture, replies, preview_texture: None, completed_texture: None, show_original: false, rendering: false, status: String::new(), size, time: 0.0, shown_time: 0.0, points: vec![], region: None, drag_from: None, result, focused: false }
    }

    fn ready(&self) -> bool {
        if self.request.face_edit.as_ref().is_some_and(|s| s.instruction.trim().is_empty()) { return false; }
        match self.request.mode {
            Mode::Points => self.points.iter().any(|p| p.include),
            Mode::Box => self.region.is_some_and(|r| r[2] >= 4 && r[3] >= 4),
        }
    }

    fn selection(&self) -> Pick {
        Pick { time: self.time, width: self.size[0], height: self.size[1], points: self.points.clone(), region: self.region, face_edit: self.request.face_edit.clone() }
    }

    fn render_preview(&mut self) {
        if self.rendering || !self.ready() { return; }
        let action = FaceEditAction::Render { pick: self.selection() };
        println!("{}", serde_json::to_string(&action).unwrap());
        let _ = std::io::stdout().flush();
        self.rendering = true;
        self.status = "Rendering preview…".into();
    }

    fn finish(&mut self, ui: &egui::Ui, accept: bool) {
        if accept {
            *self.result.borrow_mut() = Some(self.selection());
            if self.replies.is_some() {
                println!("{}", serde_json::to_string(&FaceEditAction::Finish).unwrap());
                let _ = std::io::stdout().flush();
            }
        }
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn receive_previews(&mut self, ui: &egui::Ui) {
        if let Some(receiver) = &self.replies {
            while let Ok(message) = receiver.try_recv() {
                if let Some(status) = message.status { self.status = status; }
                if let Some(error) = message.error { self.status = format!("Render failed: {error}"); self.rendering = false; self.preview_texture = self.completed_texture.clone(); }
                if let Some(path) = message.image {
                    match image::open(path) {
                        Ok(image) => {
                            let texture = ui.ctx().load_texture("face-preview", to_color_image(&image.to_rgba8()), egui::TextureOptions::LINEAR);
                            if message.complete { self.completed_texture = Some(texture.clone()); }
                            self.preview_texture = Some(texture);
                            self.show_original = false;
                            self.status = if message.complete { "Preview ready. Adjust settings and Render again, or Finish to save." } else { "Live step preview — rendering…" }.into();
                        }
                        Err(error) => self.status = format!("Could not load preview: {error}"),
                    }
                    if message.complete { self.rendering = false; }
                }
            }
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    fn undo(&mut self) {
        match self.request.mode {
            Mode::Points => {
                self.points.pop();
            }
            Mode::Box => self.region = None,
        }
    }

    /// Loads the frame at the slider's time once it has changed.
    fn refresh_frame(&mut self, ui: &egui::Ui) {
        if (self.time - self.shown_time).abs() < 1e-3 {
            return;
        }
        if let Ok(frame) = self.media.frame(self.time) {
            self.texture.set(to_color_image(&frame), egui::TextureOptions::LINEAR);
            ui.ctx().request_repaint();
        }
        self.shown_time = self.time;
    }
}

fn to_color_image(image: &image::RgbaImage) -> egui::ColorImage {
    egui::ColorImage::from_rgba_unmultiplied([image.width() as usize, image.height() as usize], image.as_raw())
}

/// Where an image `size` sits when fitted, centred, inside `area`.
pub fn fit(area: egui::Rect, size: [u32; 2]) -> egui::Rect {
    let scale = (area.width() / size[0] as f32).min(area.height() / size[1] as f32);
    egui::Rect::from_center_size(area.center(), egui::vec2(size[0] as f32 * scale, size[1] as f32 * scale))
}

/// A screen position over the fitted image, in the image's pixels, clamped to its edges.
pub fn to_image(rect: egui::Rect, size: [u32; 2], pos: egui::Pos2) -> [u32; 2] {
    let x = ((pos.x - rect.min.x) / rect.width() * size[0] as f32).clamp(0.0, size[0] as f32 - 1.0);
    let y = ((pos.y - rect.min.y) / rect.height() * size[1] as f32).clamp(0.0, size[1] as f32 - 1.0);
    [x.round() as u32, y.round() as u32]
}

fn to_screen(rect: egui::Rect, size: [u32; 2], x: u32, y: u32) -> egui::Pos2 {
    egui::pos2(rect.min.x + x as f32 / size[0] as f32 * rect.width(), rect.min.y + y as f32 / size[1] as f32 * rect.height())
}

impl eframe::App for PickerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.receive_previews(ui);
        // The word is waiting on this window, so take focus for Enter and Escape to work.
        if !self.focused {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Focus);
            self.focused = true;
        }
        let (enter, escape, undo) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
                i.key_pressed(egui::Key::Backspace) || (i.modifiers.command && i.key_pressed(egui::Key::Z)),
            )
        });
        if escape {
            return self.finish(ui, false);
        }
        let typing = ui.ctx().egui_wants_keyboard_input();
        if enter && !typing && self.ready() && !self.rendering {
            if self.replies.is_some() { self.render_preview(); }
            else { return self.finish(ui, true); }
        }
        if undo && !typing {
            self.undo();
        }

        egui::Panel::top("prompt").show(ui, |ui| {
            ui.add_space(6.0);
            ui.heading(&self.request.prompt);
            let hint = match self.request.mode {
                Mode::Points => "Left-click what to include, right-click what to leave out · Backspace undoes · Enter when done",
                Mode::Box if self.replies.is_some() => "Drag a box · Render to preview · Finish to save",
                Mode::Box => "Drag a box · drag again to redo · Enter when done",
            };
            ui.label(egui::RichText::new(hint).weak());
            ui.add_space(6.0);
        });

        let mut done = None;
        egui::Panel::bottom("actions").show(ui, |ui| {
            ui.add_space(6.0);
            if let Some(duration) = self.media.duration {
                ui.horizontal(|ui| {
                    ui.label("Frame");
                    // Sliders take their rail length from the style, not from the space given.
                    ui.spacing_mut().slider_width = (ui.available_width() - 90.0).max(120.0);
                    ui.add(egui::Slider::new(&mut self.time, 0.0..=(duration - 0.05).max(0.0)).suffix(" s").step_by(0.04));
                });
            }
            ui.horizontal(|ui| {
                if ui.button("Undo").clicked() {
                    self.undo();
                }
                if ui.button("Clear").clicked() {
                    self.points.clear();
                    self.region = None;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.replies.is_some() {
                        if ui.add_enabled(!self.rendering && self.completed_texture.is_some(), egui::Button::new("Finish")).clicked() { done = Some(true); }
                        if ui.add_enabled(!self.rendering && self.ready(), egui::Button::new("Render")).clicked() { self.render_preview(); }
                    } else if ui.add_enabled(self.ready(), egui::Button::new("Done")).clicked() { done = Some(true); }
                    if ui.button("Cancel").clicked() {
                        done = Some(false);
                    }
                });
            });
            ui.add_space(6.0);
        });
        if let Some(accept) = done {
            return self.finish(ui, accept);
        }
        self.refresh_frame(ui);

        if let Some(settings) = self.request.face_edit.as_mut() {
            egui::Panel::right("face-edit-settings").default_size(320.0).show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                ui.heading("Face replacement");
                if self.preview_texture.is_some() { ui.checkbox(&mut self.show_original, "Show original"); }
                if !self.status.is_empty() { ui.label(&self.status); }
                if self.rendering { ui.spinner(); }
                ui.label("Draw a box around the target face, including a little hair and surrounding skin.");
                if let Some(texture) = &self.reference_texture {
                    ui.label("Identity reference");
                    let scale = (280.0 / texture.size_vec2().x).min(140.0 / texture.size_vec2().y).min(1.0);
                    ui.image((texture.id(), texture.size_vec2() * scale));
                }
                ui.separator();
                ui.label("Edit instruction");
                ui.add(egui::TextEdit::multiline(&mut settings.instruction).desired_rows(7).desired_width(f32::INFINITY));
                ui.add(egui::Slider::new(&mut settings.steps, 1..=32).text("Sampling steps"));
                ui.label(egui::RichText::new("The default matches the selected model. More steps take longer.").small().weak());
                ui.add(egui::Slider::new(&mut settings.feather, 0..=64).text("Feather (px)"));
                ui.add(egui::Slider::new(&mut settings.blend, 0.0..=1.0).text("Edit blend"));
                ui.add(egui::Slider::new(&mut settings.strength, 0.25..=1.0).text("Denoising strength"));
                ui.label(egui::RichText::new("Lower strength retains more of the original face; blend mixes the finished pixels.").small().weak());
                ui.checkbox(&mut settings.oval_mask, "Face-shaped mask");
                ui.checkbox(&mut settings.live_preview, "Live step previews");
                ui.add(egui::Slider::new(&mut settings.context, 0.0..=1.0).text("Surrounding context"));
                if let Some([x, y, w, h]) = self.region.as_mut() {
                    ui.separator();
                    ui.label("Selected region (source pixels)");
                    ui.horizontal(|ui| {
                        ui.label("x"); ui.add(egui::DragValue::new(x).range(0..=self.size[0].saturating_sub(4)));
                        ui.label("y"); ui.add(egui::DragValue::new(y).range(0..=self.size[1].saturating_sub(4)));
                    });
                    *w = (*w).min(self.size[0] - *x);
                    *h = (*h).min(self.size[1] - *y);
                    ui.horizontal(|ui| {
                        ui.label("width"); ui.add(egui::DragValue::new(w).range(4..=self.size[0] - *x));
                        ui.label("height"); ui.add(egui::DragValue::new(h).range(4..=self.size[1] - *y));
                    });
                }
                ui.separator();
                ui.label("Box the face itself, not the whole head. The face-shaped mask preserves the corners, hair and background. Rendering quality and resemblance depend on the model.");
                });
            });
        }

        egui::CentralPanel::default().show(ui, |ui| {
            let (response, painter) = ui.allocate_painter(ui.available_size(), egui::Sense::click_and_drag());
            let rect = fit(response.rect, self.size);
            let texture = if !self.show_original { self.preview_texture.as_ref().unwrap_or(&self.texture) } else { &self.texture };
            painter.image(texture.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
            let inside = |pos: egui::Pos2| rect.contains(pos);
            match self.request.mode {
                Mode::Points => {
                    for outline in &self.request.outlines {
                        let area = egui::Rect::from_min_max(
                            to_screen(rect, self.size, outline[0].max(0.0) as u32, outline[1].max(0.0) as u32),
                            to_screen(rect, self.size, outline[2].max(0.0) as u32, outline[3].max(0.0) as u32),
                        );
                        painter.rect_stroke(area, 4.0, egui::Stroke::new(2.0, egui::Color32::from_rgb(80, 200, 255)), egui::StrokeKind::Outside);
                    }
                    if let Some(pos) = response.interact_pointer_pos().filter(|p| inside(*p)) {
                        let [x, y] = to_image(rect, self.size, pos);
                        if response.clicked() {
                            self.points.push(Point { x, y, include: true });
                        } else if response.secondary_clicked() {
                            self.points.push(Point { x, y, include: false });
                        }
                    }
                    for point in &self.points {
                        let at = to_screen(rect, self.size, point.x, point.y);
                        let color = if point.include { egui::Color32::from_rgb(40, 200, 90) } else { egui::Color32::from_rgb(230, 60, 60) };
                        painter.circle(at, 7.0, color, egui::Stroke::new(2.0, egui::Color32::WHITE));
                    }
                }
                Mode::Box => {
                    if response.drag_started() {
                        self.drag_from = response.interact_pointer_pos().filter(|p| inside(*p));
                    }
                    if let (Some(from), Some(to)) = (self.drag_from, response.interact_pointer_pos()) {
                        if response.dragged() || response.drag_stopped() {
                            let [x0, y0] = to_image(rect, self.size, from);
                            let [x1, y1] = to_image(rect, self.size, to);
                            self.region = Some([x0.min(x1), y0.min(y1), x0.abs_diff(x1), y0.abs_diff(y1)]);
                        }
                    }
                    if response.drag_stopped() {
                        self.drag_from = None;
                    }
                    if let Some([x, y, w, h]) = self.region {
                        let boxed = egui::Rect::from_min_max(to_screen(rect, self.size, x, y), to_screen(rect, self.size, x + w, y + h));
                        // Dim everything outside the box.
                        let shade = egui::Color32::from_black_alpha(140);
                        for part in [
                            egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, boxed.min.y)),
                            egui::Rect::from_min_max(egui::pos2(rect.min.x, boxed.max.y), rect.max),
                            egui::Rect::from_min_max(egui::pos2(rect.min.x, boxed.min.y), egui::pos2(boxed.min.x, boxed.max.y)),
                            egui::Rect::from_min_max(egui::pos2(boxed.max.x, boxed.min.y), egui::pos2(rect.max.x, boxed.max.y)),
                        ] {
                            if self.preview_texture.is_none() || self.show_original { painter.rect_filled(part, 0.0, shade); }
                        }
                        painter.rect_stroke(boxed, 0.0, egui::Stroke::new(2.0, egui::Color32::from_rgb(255, 210, 60)), egui::StrokeKind::Outside);
                        if self.request.face_edit.as_ref().is_some_and(|s| s.oval_mask) {
                            painter.add(egui::epaint::EllipseShape::stroke(boxed.center(), boxed.size() / 2.0, egui::Stroke::new(2.0, egui::Color32::from_rgb(80, 220, 255))));
                        }
                    }
                }
            }
        });
    }
}

/// Crops `image` to a pick's region and saves it as PNG.
pub fn crop(image: &Path, pick: &Pick, output: &Path) -> Result<(), String> {
    let [x, y, w, h] = pick.region.ok_or("that pick has no box; use pick-box")?;
    let source = Media::open(image)?.frame(pick.time)?;
    if (source.width(), source.height()) != (pick.width, pick.height) {
        return Err(format!(
            "the box was picked on a {}×{} image but this one is {}×{}",
            pick.width,
            pick.height,
            source.width(),
            source.height()
        ));
    }
    let cropped = image::imageops::crop_imm(&source, x, y, w.max(1), h.max(1)).to_image();
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut bytes = Cursor::new(vec![]);
    cropped.write_to(&mut bytes, image::ImageFormat::Png).map_err(|e| e.to_string())?;
    std::fs::write(output, bytes.into_inner()).map_err(|e| format!("could not save {}: {e}", output.display()))
}

/// Crops a face from a pick's box with `margin` (a fraction of the box's longer side) of
/// surroundings on every side, clamped to the image, and enlarges it with Lanczos resampling
/// until its shorter side is at least `min_side`. Saves a PNG; returns its size.
pub fn crop_face(image: &Path, pick: &Pick, output: &Path, margin: f32, min_side: u32) -> Result<(u32, u32), String> {
    let [x, y, w, h] = pick.region.ok_or("that pick has no box")?;
    let source = Media::open(image)?.frame(pick.time)?;
    let pad = (w.max(h) as f32 * margin).round() as u32;
    let (x0, y0) = (x.saturating_sub(pad), y.saturating_sub(pad));
    let x1 = (x + w + pad).min(source.width());
    let y1 = (y + h + pad).min(source.height());
    let mut face = image::DynamicImage::ImageRgba8(image::imageops::crop_imm(&source, x0, y0, (x1 - x0).max(1), (y1 - y0).max(1)).to_image());
    let short = face.width().min(face.height());
    if short < min_side {
        let scale = min_side as f32 / short as f32;
        let (width, height) = ((face.width() as f32 * scale).round() as u32, (face.height() as f32 * scale).round() as u32);
        face = face.resize_exact(width, height, image::imageops::FilterType::Lanczos3);
    }
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    face.save(output).map_err(|e| format!("could not save {}: {e}", output.display()))?;
    Ok((face.width(), face.height()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn face_crops_keep_a_margin_and_reach_the_minimum_size() {
        let dir = std::env::temp_dir().join(format!("forgeflow-face-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbaImage::from_pixel(200, 100, image::Rgba([1, 2, 3, 255])).save(dir.join("in.png")).unwrap();
        // A 40×40 box at (80, 30) with a 25% margin is 60×60 (clamped at the bottom edge to 70 tall).
        let pick = Pick { time: 0.0, width: 200, height: 100, points: vec![], face_edit: None, region: Some([80, 30, 40, 40]) };
        let (w, h) = crop_face(&dir.join("in.png"), &pick, &dir.join("face.png"), 0.25, 512).unwrap();
        assert_eq!(w.min(h), 512);
        assert_eq!(image::image_dimensions(dir.join("face.png")).unwrap(), (w, h));
        // Large crops are left at their size.
        let big = Pick { region: Some([0, 0, 200, 100]), ..pick };
        assert_eq!(crop_face(&dir.join("in.png"), &big, &dir.join("big.png"), 0.25, 64).unwrap(), (200, 100));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn maps_window_positions_to_image_pixels() {
        // A 1000×500 image in a 400×400 area is fitted to 400×200, centred vertically.
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 400.0));
        let rect = fit(area, [1000, 500]);
        assert_eq!((rect.min, rect.max), (egui::pos2(0.0, 100.0), egui::pos2(400.0, 300.0)));
        assert_eq!(to_image(rect, [1000, 500], egui::pos2(200.0, 200.0)), [500, 250]);
        assert_eq!(to_image(rect, [1000, 500], egui::pos2(-50.0, 999.0)), [0, 499]);
        assert_eq!(to_screen(rect, [1000, 500], 500, 250), egui::pos2(200.0, 200.0));
    }

    #[test]
    fn picks_round_trip_as_json_and_crop() {
        let pick = Pick { time: 1.5, width: 8, height: 6, points: vec![Point { x: 3, y: 4, include: false }], face_edit: None, region: Some([2, 1, 4, 3]) };
        let text = serde_json::to_string(&pick).unwrap();
        assert_eq!(Pick::parse(&text).unwrap(), pick);
        assert!(Pick::parse("nonsense").is_err());

        let dir = std::env::temp_dir().join(format!("forgeflow-crop-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbaImage::from_pixel(8, 6, image::Rgba([10, 20, 30, 255])).save(dir.join("in.png")).unwrap();
        let still = Pick { time: 0.0, ..pick };
        crop(&dir.join("in.png"), &still, &dir.join("out.png")).unwrap();
        assert_eq!(image::image_dimensions(dir.join("out.png")).unwrap(), (4, 3));
        let wrong = Pick { width: 9, ..still };
        assert!(crop(&dir.join("in.png"), &wrong, &dir.join("x.png")).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}

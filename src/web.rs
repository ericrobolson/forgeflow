//! `forgeflow serve`: the chat in a browser, on this machine or a phone on the same network.
//!
//! Pages are rendered in Rust with maud; htmx (embedded) posts messages and streams each
//! reply over server-sent events, so the page needs no hand-written client code beyond a
//! few lines for Enter-to-send and scrolling. Every request but the static assets needs the
//! session token, which the QR code carries and a cookie then keeps.

use std::io::{Cursor, Read, Write};
use std::net::{IpAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

use maud::{DOCTYPE, Markup, html};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::config::Config;
use crate::events::{Event, Kind, Sink, Terminal};
use crate::messages::Conversation;
use crate::workflow::Context;

const HTMX: &str = include_str!("../assets/htmx.min.js");
const HTMX_SSE: &str = include_str!("../assets/htmx-sse.js");
const STYLE: &str = include_str!("../assets/style.css");
const SCRIPT: &str = include_str!("../assets/page.js");
const COOKIE: &str = "forgeflow_token";
/// Enough for several phone photos at once, the keyframes of a video.
const MAX_UPLOAD_BYTES: usize = 160 * 1024 * 1024;
/// Uploads are scaled down to this longest side; edits work at about a megapixel anyway.
const MAX_UPLOAD_SIDE: u32 = 2048;
/// Where the web UI keeps what it creates: uploads, and images generated from the page.
pub const WEB_DIR: &str = ".tmp";
pub const UPLOAD_DIR: &str = ".tmp/uploads";
/// How often an idle stream sends a comment, so proxies and phones keep it open.
const KEEPALIVE: Duration = Duration::from_secs(15);
/// The shortest gap between updates of a streaming reply.
const UPDATE_INTERVAL: Duration = Duration::from_millis(60);

/// One message and everything the conversation emitted for it.
struct Turn {
    id: usize,
    message: String,
    mode: Mode,
    /// Photos attached to the message, in order.
    uploads: Vec<String>,
    state: Mutex<TurnState>,
    changed: Condvar,
}

#[derive(Default)]
struct TurnState {
    events: Vec<Event>,
    done: bool,
}

impl Turn {
    fn new(id: usize, message: String, mode: Mode, uploads: Vec<String>) -> Self {
        Self { id, message, mode, uploads, state: Mutex::default(), changed: Condvar::new() }
    }

    fn push(&self, event: Event) {
        self.state.lock().expect("turn lock").events.push(event);
        self.changed.notify_all();
    }

    fn finish(&self) {
        self.state.lock().expect("turn lock").done = true;
        self.changed.notify_all();
    }
}

type Current = Arc<Mutex<Option<Arc<Turn>>>>;

/// Sends events to the web turn being run, if any, and otherwise to the terminal. With
/// `mirror`, web turns are shown in the terminal too, as `forgeflow serve`'s log; in the
/// terminal chat they are not, so they never print over what is being typed.
struct WebSink {
    current: Current,
    terminal: Terminal,
    mirror: bool,
}

impl Sink for WebSink {
    fn emit(&mut self, event: Event) {
        let web_turn = self.current.lock().expect("current turn lock").clone();
        if let Some(turn) = &web_turn {
            turn.push(event.clone());
        }
        if web_turn.is_none() || self.mirror {
            self.terminal.emit(event);
        }
    }
}

/// A conversation the terminal and the web UI take turns with.
pub type SharedConversation = Arc<Mutex<Conversation>>;

/// Wraps `context` in a conversation the `serve` word can open to browsers. Turns from the
/// terminal and the web share its history, stack, project and model.
pub fn share(mut context: Context, interactive: bool, mirror: bool) -> Result<SharedConversation, String> {
    let current: Current = Arc::default();
    context.sink = Box::new(WebSink { current: current.clone(), terminal: Terminal::new(), mirror });
    let shared = Arc::new(Mutex::new(Conversation::new(context, interactive)?));
    let control = WebControl { conversation: Arc::downgrade(&shared), current, running: None };
    shared.lock().expect("conversation lock").context.web = Some(control);
    Ok(shared)
}

/// Starts and stops the web UI for the conversation that owns it.
pub struct WebControl {
    conversation: Weak<Mutex<Conversation>>,
    current: Current,
    running: Option<Running>,
}

struct Running {
    server: Arc<Server>,
    accept: Option<JoinHandle<()>>,
    port: u16,
    lan: Option<String>,
    local: String,
}

impl WebControl {
    /// Listens on all interfaces at `port` with a fresh token. Returns what to show: a QR
    /// code for the network address and the links.
    pub fn start(&mut self, port: u16, root: &Path) -> Result<String, String> {
        if let Some(running) = &self.running {
            return Ok(format!("The web UI is already running on port {}: {}", running.port, running.lan.as_ref().unwrap_or(&running.local)));
        }
        let server = Arc::new(Server::http(("0.0.0.0", port)).map_err(|e| format!("could not listen on port {port}: {e}"))?);
        let token = uuid::Uuid::new_v4().simple().to_string();
        let lan = lan_ip().map(|ip| format!("http://{ip}:{port}/?t={token}"));
        let local = format!("http://localhost:{port}/?t={token}");
        let app = Arc::new(App {
            conversation: self.conversation.clone(),
            current: self.current.clone(),
            turns: Mutex::default(),
            token,
            root: root.to_path_buf(),
        });
        let listener = server.clone();
        let accept = std::thread::spawn(move || {
            for request in listener.incoming_requests() {
                let app = app.clone();
                std::thread::spawn(move || handle(&app, request));
            }
        });
        let mut message = String::new();
        if let Some(url) = &lan {
            message.push_str(&qr_code(url)?);
            message.push_str(&format!("\nScan to open ForgeFlow on another device: {url}\n"));
        }
        message.push_str(&format!(
            "On this machine: {local}\nAnyone with the link can use this session; stop it with `\"stop\" serve` or /serve stop. If another device cannot connect, a firewall may be blocking port {port} (on Ubuntu: `sudo ufw allow {port}/tcp`)."
        ));
        self.running = Some(Running { server, accept: Some(accept), port, lan, local });
        Ok(message)
    }

    /// Stops listening. A reply that is streaming finishes, but nothing new is accepted.
    pub fn stop(&mut self) -> String {
        match self.running.take() {
            Some(running) => {
                let port = running.port;
                drop(running);
                format!("Stopped the web UI on port {port}.")
            }
            None => "The web UI is not running.".into(),
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// The accepting thread, for `forgeflow serve` to wait on.
    pub fn take_accept_thread(&mut self) -> Option<JoinHandle<()>> {
        self.running.as_mut().and_then(|r| r.accept.take())
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        // Ends `incoming_requests`, so the accepting thread returns.
        self.server.unblock();
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

struct App {
    /// Weak, so the conversation (and its llama-server) is dropped when its owner exits.
    conversation: Weak<Mutex<Conversation>>,
    current: Current,
    turns: Mutex<Vec<Arc<Turn>>>,
    token: String,
    root: PathBuf,
}

/// `forgeflow serve`: the web UI alone, with the terminal as its log, until interrupted.
pub fn serve(context: Context, port: u16) -> Result<(), String> {
    let root = context.project.root.clone();
    let shared = share(context, false, true)?;
    let accept = {
        let mut conversation = shared.lock().expect("conversation lock");
        let control = conversation.context.web.as_mut().expect("shared conversations have web control");
        println!("{}", control.start(port, &root)?);
        control.take_accept_thread()
    };
    if let Some(accept) = accept {
        let _ = accept.join();
    }
    Ok(())
}

/// The address other devices on the network reach this machine at: the source address the
/// system would use for outside traffic. Connecting a UDP socket sends nothing.
pub fn lan_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:80").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}

/// `text` as a QR code drawn with half blocks, two rows per line, light on dark so it
/// scans from a dark terminal.
pub fn qr_code(text: &str) -> Result<String, String> {
    use qrcode::render::unicode::Dense1x2;
    let code = qrcode::QrCode::new(text.as_bytes()).map_err(|e| format!("could not make a QR code: {e}"))?;
    Ok(code
        .render::<Dense1x2>()
        .dark_color(Dense1x2::Light)
        .light_color(Dense1x2::Dark)
        .quiet_zone(true)
        .build())
}

fn handle(app: &App, request: Request) {
    let url = request.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    let path = path.to_string();
    let method = request.method().clone();
    let result = match (&method, path.as_str()) {
        (Method::Get, "/assets/htmx.js") => respond(request, asset(HTMX, "text/javascript")),
        (Method::Get, "/assets/sse.js") => respond(request, asset(HTMX_SSE, "text/javascript")),
        (Method::Get, "/assets/page.js") => respond(request, asset(SCRIPT, "text/javascript")),
        (Method::Get, "/assets/style.css") => respond(request, asset(STYLE, "text/css")),
        (Method::Get, "/") if query_param(query, "t") == Some(app.token.as_str()) => {
            // Keep the token in a cookie and drop it from the address bar.
            let cookie = format!("{COOKIE}={}; Path=/; HttpOnly; SameSite=Strict", app.token);
            respond(request, Response::from_string("").with_status_code(303).with_header(header("Location", "/")).with_header(header("Set-Cookie", &cookie)))
        }
        _ if !authorized(app, &request) => respond(
            request,
            html_response(page_shell(html! { main.locked { p { "Scan the QR code in the terminal running " code { "forgeflow serve" } " to open this session." } } }))
                .with_status_code(403),
        ),
        (Method::Get, "/") => respond(request, html_response(page(app))),
        (Method::Post, "/send") => send(app, request),
        (Method::Get, p) if p.starts_with("/stream/") => stream(app, request, &p["/stream/".len()..]),
        (Method::Get, p) if p.starts_with("/files/") => file(app, request, &p["/files/".len()..]),
        _ => respond(request, Response::from_string("not found").with_status_code(404)),
    };
    if let Err(error) = result {
        eprintln!("web: {method} {path}: {error}");
    }
}

fn respond<R: Read>(request: Request, response: Response<R>) -> Result<(), String> {
    request.respond(response).map_err(|e| e.to_string())
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header")
}

fn asset(body: &'static str, content_type: &str) -> Response<Cursor<Vec<u8>>> {
    Response::from_string(body)
        .with_header(header("Content-Type", &format!("{content_type}; charset=utf-8")))
        .with_header(header("Cache-Control", "max-age=3600"))
}

fn html_response(markup: Markup) -> Response<Cursor<Vec<u8>>> {
    Response::from_string(markup.into_string()).with_header(header("Content-Type", "text/html; charset=utf-8"))
}

fn authorized(app: &App, request: &Request) -> bool {
    request
        .headers()
        .iter()
        .filter(|h| h.field.equiv("Cookie"))
        .flat_map(|h| h.value.as_str().split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .any(|(name, value)| name == COOKIE && value == app.token)
}

fn query_param<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query.split('&').filter_map(|pair| pair.split_once('=')).find(|(key, _)| *key == name).map(|(_, value)| value)
}

fn page_shell(body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover";
                meta name="color-scheme" content="light dark";
                title { "ForgeFlow" }
                link rel="stylesheet" href="/assets/style.css";
                script src="/assets/htmx.js" {}
                script src="/assets/sse.js" {}
            }
            body { (body) }
        }
    }
}

fn page(app: &App) -> Markup {
    let config = Config::load(&crate::project::Project { root: app.root.clone() }.folder()).unwrap_or_default();
    let model = config.model.unwrap_or_else(|| "no model".into());
    let turns = app.turns.lock().expect("turns lock").clone();
    page_shell(html! {
        header.bar {
            span.brand { "ForgeFlow" }
            span.model { (model) }
            button.clear type="button" title="Clear the conversation and stack"
                hx-post="/send" hx-encoding="multipart/form-data" hx-vals=r#"{"message": "/clear", "mode": "chat"}"#
                hx-target="#log" hx-swap="beforeend" hx-confirm="Clear the conversation and stack?" { "Clear context" }
        }
        main #log {
            @if turns.is_empty() {
                div.hello {
                    p { "Chat as in the terminal: " code { ";" } " runs a program, " code { "/" } " runs a command." }
                    p { "Attach a photo and say what to change to edit it." }
                }
            }
            @for turn in &turns { (turn_markup(turn)) }
        }
        form #composer hx-post="/send" hx-encoding="multipart/form-data" hx-target="#log" hx-swap="beforeend" {
            label.attach title="Attach a photo" {
                input #photo type="file" name="photo" accept="image/*" multiple hidden;
                "+"
            }
            div.field {
                div.modes role="radiogroup" aria-label="Mode" {
                    label { input type="radio" name="mode" value="chat" checked; span { "Chat" } }
                    label { input type="radio" name="mode" value="code"; span { "Code" } }
                    label { input type="radio" name="mode" value="image"; span { "Image" } }
                    label { input type="radio" name="mode" value="video"; span { "Video" } }
                    span.quality {
                        label { input type="radio" name="quality" value="draft" checked; span { "Draft" } }
                        label { input type="radio" name="quality" value="full"; span { "Full" } }
                        input.seconds type="number" name="seconds" min="0" max="600" step="1" placeholder="sec" title="Length in seconds (blank: one default segment per pair of photos)";
                    }
                }
                div #preview {}
                textarea #message name="message" rows="1" placeholder="Message" enterkeyhint="send" autocomplete="off" {}
            }
            button.send type="submit" title="Send" { "↑" }
        }
        script src="/assets/page.js" {}
    })
}

/// A turn: the user's message, then the reply, finished or still streaming.
fn turn_markup(turn: &Turn) -> Markup {
    let state = turn.state.lock().expect("turn lock");
    html! {
        div.turn {
            div class=(format!("msg user mode-{}", turn.mode.name())) {
                @if !turn.uploads.is_empty() {
                    div.thumbs {
                        @for upload in &turn.uploads {
                            a.thumb href=(file_url(upload)) target="_blank" { img src=(file_url(upload)) alt=(upload); }
                        }
                    }
                }
                @if !turn.message.is_empty() { div.text { (turn.message) } }
            }
            @if state.done {
                div.msg.bot { (render_events(&state.events)) }
            } @else {
                // Each update carries the whole reply, so Markdown renders as it streams.
                div.msg.bot hx-ext="sse" sse-connect=(format!("/stream/{}", turn.id)) sse-swap="reply" hx-swap="innerHTML" sse-close="done" {
                    span.typing { "…" }
                }
            }
        }
    }
}

/// A reply's events as HTML: runs of streamed model text as Markdown, the rest as lines.
fn render_events(events: &[Event]) -> Markup {
    let mut blocks: Vec<Markup> = vec![];
    let mut text = String::new();
    let mut text_kind = Kind::Chat;
    let flush = |blocks: &mut Vec<Markup>, text: &mut String, kind: Kind| {
        if !text.trim().is_empty() {
            blocks.push(html! { div class=(format!("md k-{}", kind.name())) { (maud::PreEscaped(markdown(text))) } });
        }
        text.clear();
    };
    for (index, event) in events.iter().enumerate() {
        // Sampling reports every step; show only the latest of a run of them.
        if is_step(event) && events.get(index + 1).is_some_and(is_step) {
            continue;
        }
        match event {
            Event::Text(kind, chunk) => {
                if *kind != text_kind {
                    flush(&mut blocks, &mut text, text_kind);
                    text_kind = *kind;
                }
                text.push_str(chunk);
            }
            other => {
                flush(&mut blocks, &mut text, text_kind);
                blocks.push(event_markup(other));
            }
        }
    }
    flush(&mut blocks, &mut text, text_kind);
    html! { @for block in &blocks { (block) } }
}

fn is_step(event: &Event) -> bool {
    matches!(event, Event::Line(Kind::Progress, text) if text.contains("step "))
}

/// Model-written Markdown as HTML. Raw HTML in it is shown as text, links only keep web
/// and relative addresses, and images become links, so a reply cannot run script or
/// load content from elsewhere.
fn markdown(text: &str) -> String {
    use pulldown_cmark::{Event as Md, Options, Parser, Tag, TagEnd};
    let safe = |url: &str| {
        let lower = url.trim().to_ascii_lowercase();
        lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with('/') || lower.starts_with('#') || !lower.contains(':')
    };
    let mut in_image = false;
    let events = Parser::new_ext(text, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS)
        .filter_map(|event| match event {
            Md::Html(raw) | Md::InlineHtml(raw) => Some(Md::Text(raw)),
            Md::Start(Tag::Image { dest_url, .. }) => {
                in_image = true;
                Some(Md::Text(format!("[image: {dest_url}]").into()))
            }
            Md::End(TagEnd::Image) => {
                in_image = false;
                None
            }
            _ if in_image => None,
            Md::Start(Tag::Link { link_type, dest_url, title, id }) => {
                let dest_url = if safe(&dest_url) { dest_url } else { "#".into() };
                Some(Md::Start(Tag::Link { link_type, dest_url, title, id }))
            }
            other => Some(other),
        });
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, events);
    html
}

fn event_markup(event: &Event) -> Markup {
    match event {
        Event::Text(kind, text) => html! { span class=(format!("k-{}", kind.name())) { (text) } },
        Event::Line(Kind::Error, text) => html! { div.line.k-error { "error: " (text) } },
        Event::Line(kind, text) => html! { div class=(format!("line k-{}", kind.name())) { (text) } },
        Event::Video(path) => {
            // Finishing a draft is one tap: upscale and smooth without regenerating.
            let finish = serde_json::json!({ "message": format!("; {} finish-video drop", quote(path)), "mode": "code" }).to_string();
            html! {
                figure.image {
                    video src=(file_url(path)) poster=(file_url(&Path::new(path).with_extension("jpg").to_string_lossy())) controls playsinline preload="metadata" {}
                    figcaption {
                        a href=(file_url(path)) target="_blank" { (path) }
                        @if !path.ends_with("-finished.mp4") {
                            button.finish type="button" title="Upscale to 1080p and double the frame rate"
                                hx-post="/send" hx-encoding="multipart/form-data" hx-vals=(finish) hx-target="#log" hx-swap="beforeend" { "Finish: 1080p · 2× fps" }
                        }
                    }
                }
            }
        }
        Event::Image(path) => html! {
            figure.image {
                a href=(file_url(path)) target="_blank" { img src=(file_url(path)) alt=(path) loading="lazy"; }
                figcaption { (path) }
            }
        },
    }
}

fn file_url(relative: &str) -> String {
    let encoded: String = relative
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    format!("/files/{encoded}")
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&text[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Starts a turn from the posted form and answers with its markup; the reply streams in.
fn send(app: &App, mut request: Request) -> Result<(), String> {
    let content_type = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Content-Type"))
        .map(|h| h.value.as_str().to_string())
        .unwrap_or_default();
    let mut body = Vec::new();
    request
        .as_reader()
        .take(MAX_UPLOAD_BYTES as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|e| e.to_string())?;
    let reply = |request: Request, markup: Markup| respond(request, html_response(markup));
    if body.len() > MAX_UPLOAD_BYTES {
        return reply(request, notice("That upload is too large (the limit is 160 MB)."));
    }
    let Some(boundary) = content_type.split("boundary=").nth(1).map(|b| b.trim_matches('"').to_string()) else {
        return reply(request, notice("Expected a multipart form."));
    };
    let parts = parse_multipart(&body, &boundary);
    let message = parts
        .iter()
        .find(|p| p.name == "message")
        .map(|p| String::from_utf8_lossy(&p.data).trim().to_string())
        .unwrap_or_default();
    let mode = parts.iter().find(|p| p.name == "mode").map_or(Mode::Chat, |p| Mode::parse(&String::from_utf8_lossy(&p.data)));
    let quality = parts.iter().find(|p| p.name == "quality").map(|p| String::from_utf8_lossy(&p.data).trim().to_string());
    let seconds = parts.iter().find(|p| p.name == "seconds").and_then(|p| String::from_utf8_lossy(&p.data).trim().parse::<u32>().ok());
    let photos: Vec<&Part> = parts.iter().filter(|p| p.name == "photo" && !p.data.is_empty()).collect();
    if message.is_empty() && photos.is_empty() {
        return reply(request, html! {});
    }
    if app.current.lock().expect("current turn lock").is_some() {
        return reply(request, notice("Still working on the previous message."));
    }
    let uploads = match photos.iter().map(|p| save_upload(&app.root, &p.data)).collect::<Result<Vec<_>, _>>() {
        Ok(uploads) => uploads,
        Err(error) => return reply(request, notice(&error)),
    };

    let turn = {
        let mut turns = app.turns.lock().expect("turns lock");
        let turn = Arc::new(Turn::new(turns.len(), message.clone(), mode, uploads.clone()));
        turns.push(turn.clone());
        turn
    };
    *app.current.lock().expect("current turn lock") = Some(turn.clone());
    let markup = turn_markup(&turn);
    let program = turn_program(&message, mode, &uploads, quality.as_deref(), seconds, &app.root);
    // The turn runs on this request's thread after the response is sent, so the page
    // shows the message at once and the reply streams in. A terminal turn in progress
    // finishes first.
    reply(request, markup)?;
    match (program, app.conversation.upgrade()) {
        (Some(text), Some(conversation)) => conversation.lock().expect("conversation lock").turn(&text),
        (Some(_), None) => turn.push(Event::Line(Kind::Error, "the session has ended".into())),
        (None, _) => turn.push(Event::Line(Kind::Note, format!("Saved {}. Mention it in a message to use it.", uploads.join(", ")))),
    }
    *app.current.lock().expect("current turn lock") = None;
    turn.finish();
    Ok(())
}

/// The composer's mode buttons: chat as in the terminal, run code, make an image, or
/// make a video.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Chat,
    Code,
    Image,
    Video,
}

impl Mode {
    fn parse(text: &str) -> Self {
        match text.trim() {
            "code" => Mode::Code,
            "image" => Mode::Image,
            "video" => Mode::Video,
            _ => Mode::Chat,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Mode::Chat => "chat",
            Mode::Code => "code",
            Mode::Image => "image",
            Mode::Video => "video",
        }
    }
}

/// What a posted message runs. Attached photos with text edit the first photo (the rest are
/// references), unless the text is code or a command; photos alone are only saved. Code mode
/// runs the text as a program; image mode generates the text as a new image in `.tmp/`;
/// video mode makes a video through the photos, in order, at the chosen quality.
fn turn_program(message: &str, mode: Mode, uploads: &[String], quality: Option<&str>, seconds: Option<u32>, root: &Path) -> Option<String> {
    if message.is_empty() {
        return None;
    }
    let edit = || format!("; {} {} edit-image drop", quote(&uploads.join(",")), quote(message));
    match mode {
        Mode::Code => Some(format!("; {}", message.strip_prefix(';').unwrap_or(message).trim())),
        Mode::Video => {
            let quality = match quality {
                Some("full") => "full",
                _ => "draft",
            };
            let seconds = seconds.unwrap_or(0).min(600);
            let output = quote(&next_path(root, "video", "mp4"));
            // Without photos, draft-video makes its own keyframes so characters stay consistent.
            if uploads.is_empty() && quality == "draft" {
                return Some(format!("; {seconds} video-seconds {output} {} draft-video drop", quote(message)));
            }
            Some(format!(
                "; {} video-quality {seconds} video-seconds {output} {} {} make-video drop",
                quote(quality),
                quote(&uploads.join(",")),
                quote(message)
            ))
        }
        Mode::Image if !uploads.is_empty() => Some(edit()),
        Mode::Image => Some(format!("; {} {} make-image drop", quote(&next_path(root, "image", "png")), quote(message))),
        Mode::Chat if !uploads.is_empty() && !message.starts_with([';', '/', '?']) => Some(edit()),
        Mode::Chat => Some(message.to_string()),
    }
}

/// `.tmp/NAME-N.EXT` with the first N not taken.
fn next_path(root: &Path, name: &str, extension: &str) -> String {
    (1..)
        .map(|n| format!("{WEB_DIR}/{name}-{n}.{extension}"))
        .find(|path| !root.join(path).exists())
        .expect("a free name")
}

/// `text` as a ForgeFlow string literal.
fn quote(text: &str) -> String {
    let escaped = text.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\r', "").replace('\t', "\\t");
    format!("\"{escaped}\"")
}

fn notice(text: &str) -> Markup {
    html! { div.turn { div.msg.bot { div.line.k-error { (text) } } } }
}

struct Part {
    name: String,
    data: Vec<u8>,
}

/// The named parts of a `multipart/form-data` body.
fn parse_multipart(body: &[u8], boundary: &str) -> Vec<Part> {
    let delimiter = format!("--{boundary}").into_bytes();
    let mut parts = vec![];
    let mut sections = split_bytes(body, &delimiter).into_iter().skip(1);
    for section in sections.by_ref() {
        // The closing delimiter is followed by `--`.
        if section.starts_with(b"--") {
            break;
        }
        let section = section.strip_prefix(b"\r\n").unwrap_or(section);
        let Some(split) = find(section, b"\r\n\r\n") else { continue };
        let headers = String::from_utf8_lossy(&section[..split]);
        let data = &section[split + 4..];
        let data = data.strip_suffix(b"\r\n").unwrap_or(data);
        let name = headers
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("content-disposition"))
            .and_then(|l| l.split(';').map(str::trim).find_map(|p| p.strip_prefix("name=")))
            .map(|n| n.trim_matches('"').to_string());
        if let Some(name) = name {
            parts.push(Part { name, data: data.to_vec() });
        }
    }
    parts
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn split_bytes<'a>(mut body: &'a [u8], delimiter: &[u8]) -> Vec<&'a [u8]> {
    let mut out = vec![];
    while let Some(at) = find(body, delimiter) {
        out.push(&body[..at]);
        body = &body[at + delimiter.len()..];
    }
    out.push(body);
    out
}

/// Saves an uploaded photo as `.tmp/uploads/photo-N.png`: upright (phones record rotation
/// separately), at most `MAX_UPLOAD_SIDE` on its longest side, and as PNG, which every
/// local model reads. Returns its project-relative path.
fn save_upload(root: &Path, bytes: &[u8]) -> Result<String, String> {
    use image::ImageDecoder;
    let unreadable = |e: image::ImageError| format!("could not read that image ({e}); try a JPEG or PNG");
    let mut decoder = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .into_decoder()
        .map_err(unreadable)?;
    let orientation = decoder.orientation().map_err(unreadable)?;
    let mut picture = image::DynamicImage::from_decoder(decoder).map_err(unreadable)?;
    picture.apply_orientation(orientation);
    if picture.width().max(picture.height()) > MAX_UPLOAD_SIDE {
        picture = picture.resize(MAX_UPLOAD_SIDE, MAX_UPLOAD_SIDE, image::imageops::FilterType::Lanczos3);
    }
    let dir = root.join(UPLOAD_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let name = (1..).map(|n| format!("photo-{n}.png")).find(|name| !dir.join(name).exists()).expect("a free name");
    picture.save(dir.join(&name)).map_err(|e| format!("could not save the upload: {e}"))?;
    Ok(format!("{UPLOAD_DIR}/{name}"))
}

/// Streams a turn as server-sent events, each carrying the whole reply so far, resuming after `Last-Event-ID` when the
/// browser reconnects, and ends with `done`.
fn stream(app: &App, request: Request, id: &str) -> Result<(), String> {
    let turn = id.parse::<usize>().ok().and_then(|id| app.turns.lock().expect("turns lock").get(id).cloned());
    let Some(turn) = turn else {
        return respond(request, Response::from_string("no such turn").with_status_code(404));
    };
    let mut next = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Last-Event-ID"))
        .and_then(|h| h.value.as_str().parse::<usize>().ok())
        // Each update's id is how many events it shows, so resume after that many.
        .unwrap_or(0);
    // tiny_http buffers chunked bodies, so write the stream directly and flush each event.
    let mut out = request.into_writer();
    out.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n")
        .map_err(|e| e.to_string())?;
    loop {
        // Read the events and the done flag together, so `finished` means the last update
        // holds every event.
        let (events, finished) = {
            let mut state = turn.state.lock().expect("turn lock");
            if next >= state.events.len() && !state.done {
                state = turn.changed.wait_timeout(state, KEEPALIVE).expect("turn lock").0;
                // Let a burst of tokens gather, so each update re-renders the reply once.
                if !state.done {
                    drop(state);
                    std::thread::sleep(UPDATE_INTERVAL);
                    state = turn.state.lock().expect("turn lock");
                }
            }
            (state.events.clone(), state.done)
        };
        let mut chunk = String::new();
        if events.len() > next || (finished && next == 0) {
            next = events.len();
            chunk.push_str(&sse_frame(Some(next), "reply", &render_events(&events).into_string()));
        } else if !finished {
            chunk.push_str(": keepalive\n\n");
        }
        if finished {
            chunk.push_str(&sse_frame(None, "done", "done"));
        }
        // A failed write means the page closed; the turn keeps running regardless.
        if out.write_all(chunk.as_bytes()).and_then(|()| out.flush()).is_err() || finished {
            return Ok(());
        }
    }
}

/// One server-sent event; each line of `data` gets its own `data:` field.
fn sse_frame(id: Option<usize>, event: &str, data: &str) -> String {
    let mut frame = String::new();
    if let Some(id) = id {
        frame.push_str(&format!("id: {id}\n"));
    }
    frame.push_str(&format!("event: {event}\n"));
    for line in data.split('\n') {
        frame.push_str("data: ");
        frame.push_str(line);
        frame.push('\n');
    }
    frame.push('\n');
    frame
}

/// Serves an image or video from the project; other files stay private. Byte ranges are
/// supported, as phones require them to play video.
fn file(app: &App, request: Request, encoded: &str) -> Result<(), String> {
    let relative = percent_decode(encoded);
    let content_type = match Path::new(&relative).extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        _ => return respond(request, Response::from_string("only images and videos are served").with_status_code(403)),
    };
    let project = crate::project::Project { root: app.root.clone() };
    let path = match project.resolve(&relative) {
        Ok(path) if path.is_file() => path,
        _ => return respond(request, Response::from_string("not found").with_status_code(404)),
    };
    let mut file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
    let length = file.metadata().map_err(|e| e.to_string())?.len();
    let range = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Range"))
        .and_then(|h| parse_range(h.value.as_str(), length));
    let Some((start, end)) = range else {
        return respond(
            request,
            Response::from_file(file)
                .with_header(header("Content-Type", content_type))
                .with_header(header("Accept-Ranges", "bytes"))
                .with_header(header("Cache-Control", "no-cache")),
        );
    };
    use std::io::Seek;
    file.seek(std::io::SeekFrom::Start(start)).map_err(|e| e.to_string())?;
    let size = end - start + 1;
    respond(
        request,
        Response::new(
            tiny_http::StatusCode(206),
            vec![
                header("Content-Type", content_type),
                header("Accept-Ranges", "bytes"),
                header("Content-Range", &format!("bytes {start}-{end}/{length}")),
                header("Cache-Control", "no-cache"),
            ],
            file.take(size),
            Some(size as usize),
            None,
        ),
    )
}

/// A single `bytes=` range within a file of `length` bytes, as inclusive (start, end).
fn parse_range(value: &str, length: u64) -> Option<(u64, u64)> {
    let spec = value.trim().strip_prefix("bytes=")?;
    if spec.contains(',') || length == 0 {
        return None;
    }
    let (start, end) = spec.split_once('-')?;
    let (start, end) = match (start.trim(), end.trim()) {
        ("", suffix) => {
            let suffix: u64 = suffix.parse().ok()?;
            (length.saturating_sub(suffix), length - 1)
        }
        (start, "") => (start.parse().ok()?, length - 1),
        (start, end) => (start.parse().ok()?, end.parse::<u64>().ok()?.min(length - 1)),
    };
    (start <= end && start < length).then_some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_multipart_form_with_a_file() {
        let body = b"--XyZ\r\nContent-Disposition: form-data; name=\"message\"\r\n\r\nmake it night\r\n--XyZ\r\nContent-Disposition: form-data; name=\"photo\"; filename=\"a.jpg\"\r\nContent-Type: image/jpeg\r\n\r\n\x89PNG\r\n\x00\x01\r\n--XyZ--\r\n";
        let parts = parse_multipart(body, "XyZ");
        assert_eq!(parts.len(), 2);
        assert_eq!((parts[0].name.as_str(), parts[0].data.as_slice()), ("message", &b"make it night"[..]));
        assert_eq!((parts[1].name.as_str(), parts[1].data.as_slice()), ("photo", &b"\x89PNG\r\n\x00\x01"[..]));
    }

    #[test]
    fn modes_and_photos_choose_the_program() {
        let root = std::env::temp_dir().join(format!("forgeflow-modes-{}", uuid::Uuid::now_v7()));
        let one = vec![".tmp/uploads/photo-1.png".to_string()];
        let two = vec![".tmp/uploads/photo-1.png".to_string(), ".tmp/uploads/photo-2.png".to_string()];
        let run = |message, mode, uploads: &[String], quality| turn_program(message, mode, uploads, quality, None, &root);
        assert_eq!(
            run("add \"rain\"\nnow", Mode::Chat, &one, None).as_deref(),
            Some(r#"; ".tmp/uploads/photo-1.png" "add \"rain\"\nnow" edit-image drop"#)
        );
        assert_eq!(run("", Mode::Chat, &one, None), None);
        assert_eq!(run("; 2 3 +", Mode::Chat, &one, None).as_deref(), Some("; 2 3 +"));
        assert_eq!(run("hello", Mode::Chat, &[], None).as_deref(), Some("hello"));
        assert_eq!(run("2 3 + print", Mode::Code, &[], None).as_deref(), Some("; 2 3 + print"));
        assert_eq!(run(";2 3 +", Mode::Code, &[], None).as_deref(), Some("; 2 3 +"));
        assert_eq!(
            run("a gladiator at dawn", Mode::Image, &[], None).as_deref(),
            Some(r#"; ".tmp/image-1.png" "a gladiator at dawn" make-image drop"#)
        );
        assert_eq!(
            run("make it dusk", Mode::Image, &two, None).as_deref(),
            Some(r#"; ".tmp/uploads/photo-1.png,.tmp/uploads/photo-2.png" "make it dusk" edit-image drop"#)
        );
        assert_eq!(
            run("they fight", Mode::Video, &two, Some("full")).as_deref(),
            Some(r#"; "full" video-quality 0 video-seconds ".tmp/video-1.mp4" ".tmp/uploads/photo-1.png,.tmp/uploads/photo-2.png" "they fight" make-video drop"#)
        );
        assert_eq!(
            run("a storm", Mode::Video, &[], None).as_deref(),
            Some(r#"; 0 video-seconds ".tmp/video-1.mp4" "a storm" draft-video drop"#)
        );
        assert_eq!(
            turn_program("a storm", Mode::Video, &[], None, Some(7), &root).as_deref(),
            Some(r#"; 7 video-seconds ".tmp/video-1.mp4" "a storm" draft-video drop"#)
        );
        std::fs::create_dir_all(root.join(".tmp")).unwrap();
        std::fs::write(root.join(".tmp/image-1.png"), b"").unwrap();
        assert_eq!(next_path(&root, "image", "png"), ".tmp/image-2.png");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn byte_ranges_follow_rfc_7233() {
        assert_eq!(parse_range("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(parse_range("bytes=500-", 1000), Some((500, 999)));
        assert_eq!(parse_range("bytes=-100", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=900-5000", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=1000-", 1000), None);
        assert_eq!(parse_range("bytes=0-1,5-9", 1000), None);
    }

    #[test]
    fn only_the_latest_sampling_step_is_shown() {
        let step = |n: u32| Event::Line(Kind::Progress, format!("step {n}/6"));
        let html = render_events(&[step(1), step(2), step(3), Event::Line(Kind::Progress, "Done".into())]).into_string();
        assert!(!html.contains("step 1/6") && !html.contains("step 2/6") && html.contains("step 3/6") && html.contains("Done"));
    }

    #[test]
    fn server_sent_events_split_lines_and_carry_ids() {
        assert_eq!(sse_frame(Some(3), "delta", "a\nb"), "id: 3\nevent: delta\ndata: a\ndata: b\n\n");
        assert_eq!(sse_frame(None, "done", "done"), "event: done\ndata: done\n\n");
    }

    #[test]
    fn file_urls_round_trip() {
        let path = "shots/my arena #1.png";
        let url = file_url(path);
        assert_eq!(url, "/files/shots/my%20arena%20%231.png");
        assert_eq!(percent_decode(&url["/files/".len()..]), path);
    }

    #[test]
    fn events_render_escaped() {
        let html = event_markup(&Event::Line(Kind::Run, "<b>hi</b>".into())).into_string();
        assert_eq!(html, "<div class=\"line k-run\">&lt;b&gt;hi&lt;/b&gt;</div>");
    }

    #[test]
    fn streamed_text_renders_as_markdown_between_lines() {
        let events = [
            Event::Line(Kind::Note, "→ program".into()),
            Event::Text(Kind::Chat, "## Fighters\n\n- **Spar".into()),
            Event::Text(Kind::Chat, "tacus**\n- `Crixus`\n".into()),
            Event::Line(Kind::Note, "~100 tokens left".into()),
        ];
        let html = render_events(&events).into_string();
        assert!(html.starts_with("<div class=\"line k-note\">→ program</div><div class=\"md k-chat\"><h2>Fighters</h2>"));
        assert!(html.contains("<li><strong>Spartacus</strong></li>"));
        assert!(html.contains("<code>Crixus</code>"));
        assert!(html.ends_with("<div class=\"line k-note\">~100 tokens left</div>"));
    }

    #[test]
    fn markdown_cannot_inject_script_or_remote_images() {
        let html = markdown("<script>alert(1)</script>\n\n[x](javascript:alert(1)) [ok](https://example.com) ![p](https://evil.test/a.png)");
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains(r##"<a href="#">x</a>"##));
        assert!(html.contains(r#"<a href="https://example.com">ok</a>"#));
        assert!(!html.contains("<img"));
        assert!(html.contains("[image: https://evil.test/a.png]"));
    }

    #[test]
    fn qr_codes_render_as_text() {
        let qr = qr_code("http://192.168.1.2:8090/?t=abc").unwrap();
        assert!(qr.lines().count() > 10);
    }
}

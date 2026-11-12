//! Transport shared by the chat web UI and the RPG web UI: QR codes, tokens, multipart, Markdown, files, and server-sent events.

use maud::{DOCTYPE, Markup, html};
use std::{
    io::{Cursor, Read, Write},
    net::{IpAddr, UdpSocket},
    path::Path,
    time::Duration,
};
use tiny_http::{Header, Request, Response};
/// Formats seconds since the Unix epoch as a UTC calendar timestamp.
pub fn utc_stamp(secs: u64) -> String {
    let (days, rest) = (secs / 86_400, secs % 86_400);
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

use crate::events::Event;
use std::sync::{Condvar, Mutex};
/// One reply's events and completion signal for SSE readers.
#[derive(Default)]
pub struct Feed {
    pub state: Mutex<FeedState>,
    pub changed: Condvar,
}
/// An immutable snapshot of one reply's events.
#[derive(Default, Clone)]
pub struct FeedState {
    pub events: Vec<Event>,
    pub done: bool,
}
impl Feed {
    /// Appends an event and wakes waiting readers.
    pub fn push(&self, event: Event) {
        self.state.lock().expect("turn lock").events.push(event);
        self.changed.notify_all();
    }
    /// Marks the reply complete and wakes waiting readers.
    pub fn finish(&self) {
        self.state.lock().expect("turn lock").done = true;
        self.changed.notify_all();
    }
    /// Copies events and completion under the feed lock.
    pub fn snapshot(&self) -> FeedState {
        self.state.lock().expect("turn lock").clone()
    }
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
    let code = qrcode::QrCode::new(text.as_bytes())
        .map_err(|e| format!("could not make a QR code: {e}"))?;
    Ok(code
        .render::<Dense1x2>()
        .dark_color(Dense1x2::Light)
        .light_color(Dense1x2::Dark)
        .quiet_zone(true)
        .build())
}

/// Sends an HTTP response, reporting transport failures.
pub fn respond<R: Read>(request: Request, response: Response<R>) -> Result<(), String> {
    request.respond(response).map_err(|e| e.to_string())
}

/// Builds a header from known HTTP header text.
pub fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header")
}

/// Adds a content-derived version so browsers fetch changed static assets after an update.
pub fn asset_url(path: &str, content: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hash);
    format!("{path}?v={:x}", hash.finish())
}

/// Serves an embedded static asset with a cache header.
pub fn asset(body: &'static str, content_type: &str) -> Response<Cursor<Vec<u8>>> {
    Response::from_string(body)
        .with_header(header(
            "Content-Type",
            &format!("{content_type}; charset=utf-8"),
        ))
        .with_header(header("Cache-Control", "max-age=3600"))
}

/// Wraps rendered markup in an HTML response.
pub fn html_response(markup: Markup) -> Response<Cursor<Vec<u8>>> {
    Response::from_string(markup.into_string())
        .with_header(header("Content-Type", "text/html; charset=utf-8"))
}

/// Checks the session cookie on an HTTP request.
pub fn authorized(token: &str, request: &Request) -> bool {
    cookie_has_token(
        &request
            .headers()
            .iter()
            .filter(|h| h.field.equiv("Cookie"))
            .map(|h| h.value.as_str())
            .collect::<Vec<_>>(),
        token,
    )
}
/// Matches a complete token cookie among one or more cookie headers.
pub fn cookie_has_token(cookie_headers: &[&str], token: &str) -> bool {
    cookie_headers
        .iter()
        .flat_map(|h| h.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .any(|(name, value)| name == COOKIE && value == token)
}

/// Finds a named query value without decoding it.
pub fn query_param<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value)
}

/// Wraps page contents with shared metadata, styles and htmx scripts.
pub fn page_shell(body: Markup) -> Markup {
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

/// Model-written Markdown as HTML. Raw HTML in it is shown as text, links only keep web
/// and relative addresses, and images become links, so a reply cannot run script or
/// load content from elsewhere.
pub fn markdown(text: &str) -> String {
    use pulldown_cmark::{Event as Md, Options, Parser, Tag, TagEnd};
    let safe = |url: &str| {
        let lower = url.trim().to_ascii_lowercase();
        lower.starts_with("http://")
            || lower.starts_with("https://")
            || lower.starts_with('/')
            || lower.starts_with('#')
            || !lower.contains(':')
    };
    let mut in_image = false;
    let events = Parser::new_ext(
        text,
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS,
    )
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
        Md::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => {
            let dest_url = if safe(&dest_url) {
                dest_url
            } else {
                "#".into()
            };
            Some(Md::Start(Tag::Link {
                link_type,
                dest_url,
                title,
                id,
            }))
        }
        other => Some(other),
    });
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, events);
    html
}

/// Encodes a project-relative media path as a file route.
pub fn file_url(relative: &str) -> String {
    let encoded: String = relative
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect();
    format!("/files/{encoded}")
}

/// Decodes percent-encoded path bytes.
pub fn percent_decode(text: &str) -> String {
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

/// Renders a short request failure in the conversation log.
pub fn notice(text: &str) -> Markup {
    html! { div.turn { div.msg.bot { div.line.k-error { (text) } } } }
}

/// The named parts of a `multipart/form-data` body.
pub fn parse_multipart(body: &[u8], boundary: &str) -> Vec<Part> {
    let delimiter = format!("--{boundary}").into_bytes();
    let mut parts = vec![];
    let mut sections = split_bytes(body, &delimiter).into_iter().skip(1);
    for section in sections.by_ref() {
        // The closing delimiter is followed by `--`.
        if section.starts_with(b"--") {
            break;
        }
        let section = section.strip_prefix(b"\r\n").unwrap_or(section);
        let Some(split) = find(section, b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&section[..split]);
        let data = &section[split + 4..];
        let data = data.strip_suffix(b"\r\n").unwrap_or(data);
        let name = headers
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("content-disposition"))
            .and_then(|l| {
                l.split(';')
                    .map(str::trim)
                    .find_map(|p| p.strip_prefix("name="))
            })
            .map(|n| n.trim_matches('"').to_string());
        if let Some(name) = name {
            parts.push(Part {
                name,
                data: data.to_vec(),
            });
        }
    }
    parts
}

/// Finds a byte sequence in a larger byte slice.
pub fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Splits bytes around a delimiter without decoding them.
pub fn split_bytes<'a>(mut body: &'a [u8], delimiter: &[u8]) -> Vec<&'a [u8]> {
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
pub fn save_upload(root: &Path, bytes: &[u8]) -> Result<String, String> {
    use image::ImageDecoder;
    let unreadable =
        |e: image::ImageError| format!("could not read that image ({e}); try a JPEG or PNG");
    let mut decoder = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .into_decoder()
        .map_err(unreadable)?;
    let orientation = decoder.orientation().map_err(unreadable)?;
    let mut picture = image::DynamicImage::from_decoder(decoder).map_err(unreadable)?;
    picture.apply_orientation(orientation);
    if picture.width().max(picture.height()) > MAX_UPLOAD_SIDE {
        picture = picture.resize(
            MAX_UPLOAD_SIDE,
            MAX_UPLOAD_SIDE,
            image::imageops::FilterType::Lanczos3,
        );
    }
    let dir = root.join(UPLOAD_DIR);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let name = (1..)
        .map(|n| format!("photo-{n}.png"))
        .find(|name| !dir.join(name).exists())
        .expect("a free name");
    picture
        .save(dir.join(&name))
        .map_err(|e| format!("could not save the upload: {e}"))?;
    Ok(format!("{UPLOAD_DIR}/{name}"))
}

/// One server-sent event; each line of `data` gets its own `data:` field.
pub fn sse_frame(id: Option<usize>, event: &str, data: &str) -> String {
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
pub fn serve_file(root: &Path, request: Request, encoded: &str) -> Result<(), String> {
    let relative = percent_decode(encoded);
    let content_type = match Path::new(&relative)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mov") => "video/quicktime",
        Some("mkv") => "video/x-matroska",
        _ => {
            return respond(
                request,
                Response::from_string("only images and videos are served").with_status_code(403),
            );
        }
    };
    let project = crate::project::Project {
        root: root.to_path_buf(),
    };
    let path = match project.resolve(&relative) {
        Ok(path) if path.is_file() => path,
        _ => {
            return respond(
                request,
                Response::from_string("not found").with_status_code(404),
            );
        }
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
    file.seek(std::io::SeekFrom::Start(start))
        .map_err(|e| e.to_string())?;
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
pub fn parse_range(value: &str, length: u64) -> Option<(u64, u64)> {
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
        (start, end) => (
            start.parse().ok()?,
            end.parse::<u64>().ok()?.min(length - 1),
        ),
    };
    (start <= end && start < length).then_some((start, end))
}

/// Seconds since 1970 as `YYYYMMDD-HHMMSS` in UTC (civil-from-days, Howard Hinnant's method).

/// `.tmp/NAME-N.EXT` with the first N not taken.
pub fn next_path(root: &Path, name: &str, extension: &str) -> String {
    (1..)
        .map(|n| format!("{WEB_DIR}/{name}-{n}.{extension}"))
        .find(|path| !root.join(path).exists())
        .expect("a free name")
}

/// One named multipart form field and its raw bytes.
pub struct Part {
    pub name: String,
    pub data: Vec<u8>,
}

/// The embedded htmx client.
pub const HTMX: &str = include_str!("../assets/htmx.min.js");

/// The embedded htmx SSE extension.
pub const HTMX_SSE: &str = include_str!("../assets/htmx-sse.js");

/// Shared phone and desktop web styles.
pub const STYLE: &str = include_str!("../assets/style.css");

/// The authentication cookie name.
pub const COOKIE: &str = "forgeflow_token";

/// The maximum accepted multipart request size in bytes.
pub const MAX_UPLOAD_BYTES: usize = 160 * 1024 * 1024;

/// The maximum saved upload dimension in pixels.
pub const MAX_UPLOAD_SIDE: u32 = 2048;

/// Project-relative folder for web media.
pub const WEB_DIR: &str = ".tmp";

/// Project-relative folder for uploaded images.
pub const UPLOAD_DIR: &str = ".tmp/uploads";

/// How often idle SSE connections send a keepalive comment.
pub const KEEPALIVE: Duration = Duration::from_secs(15);

/// The minimum interval between streamed reply renders.
pub const UPDATE_INTERVAL: Duration = Duration::from_millis(60);

/// Streams reply snapshots as SSE, resuming after Last-Event-ID.
pub fn stream_feed(
    request: Request,
    feed: &Feed,
    render: &dyn Fn(&[Event]) -> Markup,
) -> Result<(), String> {
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
            let mut state = feed.state.lock().expect("turn lock");
            if next >= state.events.len() && !state.done {
                state = feed
                    .changed
                    .wait_timeout(state, KEEPALIVE)
                    .expect("turn lock")
                    .0;
                // Let a burst of tokens gather, so each update re-renders the reply once.
                if !state.done {
                    drop(state);
                    std::thread::sleep(UPDATE_INTERVAL);
                    state = feed.state.lock().expect("turn lock");
                }
            }
            (state.events.clone(), state.done)
        };
        let mut chunk = String::new();
        if events.len() > next || (finished && next == 0) {
            next = events.len();
            chunk.push_str(&sse_frame(
                Some(next),
                "reply",
                &render(&events).into_string(),
            ));
        } else if !finished {
            chunk.push_str(": keepalive\n\n");
        }
        if finished {
            chunk.push_str(&sse_frame(None, "done", "done"));
        }
        // A failed write means the page closed; the turn keeps running regardless.
        if out
            .write_all(chunk.as_bytes())
            .and_then(|()| out.flush())
            .is_err()
            || finished
        {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn feed_snapshot_sees_pushes_and_finish() {
        let f = Feed::default();
        f.push(crate::events::Event::Text(
            crate::events::Kind::Chat,
            "story".into(),
        ));
        assert_eq!(f.snapshot().events.len(), 1);
        assert!(!f.snapshot().done);
        f.finish();
        assert!(f.snapshot().done);
    }
    #[test]
    fn cookie_has_token_matches_only_the_token() {
        assert!(cookie_has_token(&["a=1; forgeflow_token=abc"], "abc"));
        assert!(!cookie_has_token(&["a=1; forgeflow_token=abc"], "abd"));
        assert!(!cookie_has_token(&[], "abc"));
    }
}

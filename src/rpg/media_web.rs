//! Authenticated phone media pages and background job endpoints.
use super::{
    media::{self, FaceBox, Scan},
    web::{App, RpgTurn, finish, start_turn},
};
use crate::{
    events::{Event, Kind},
    web_util::*,
    workflow::Context,
};
use maud::{Markup, html};
use std::io::Read;
use tiny_http::{Method, Request, Response};

pub(super) struct Runners {
    pub combine: fn(&mut Context, &[String], f64) -> Result<String, String>,
    pub scan: fn(&mut Context, &str) -> Result<Scan, String>,
    pub swap: fn(&mut Context, &str, &str, &FaceBox, usize) -> Result<String, String>,
}
impl Default for Runners {
    fn default() -> Self {
        Self {
            combine: media::combine,
            scan: media::scan,
            swap: media::swap,
        }
    }
}
pub(super) fn menu() -> Markup {
    html! {nav.phone-nav aria-label="Phone navigation" {a.media-link href="/"{"Chat"} a.media-link href="/media"{"Media"} a.media-link href="/roleplay"{"Roleplay"} details.phone-menu{summary{"Menu"}nav{a href="/"{"Story"}a href="/media#face-swap"{"Face swap"}}}}}
}
fn page() -> Markup {
    page_shell(html! {
        link rel="stylesheet" href=(asset_url("/assets/media.css", include_str!("../../assets/media.css")));
        header.bar{span.brand{"Media"}(menu())}
        main.media-page{
            p{"Images and videos saved in this project's .tmp folder. Select files to combine, or explicitly choose a source face and target."}
            form #upload-form{label{"Add images or videos " input #media-upload type="file" accept="image/png,image/jpeg,image/webp,video/mp4,video/webm" multiple;}button type="submit"{"Upload"}}
            div.controls{label{"Show " select #media-filter{option value="all"{"All"}option value="image"{"Images"}option value="video"{"Videos"}}}button type="button" data-refresh{"Refresh"}span #media-count role="status"{}}
            p #media-error role="alert"{}
            section #media-grid aria-label="Saved media"{}
            nav.paging aria-label="Gallery pages"{
                button #first-page type="button"{"First"} button #back-two type="button"{"Back 2"} button #previous type="button"{"Previous"}
                span #page-links{} span #page-number{}
                button #next type="button"{"Next"} button #skip-two type="button"{"Skip 2"} button #last-page type="button"{"Last"}
                form #page-jump{label{"Page " input #page-input type="number" min="1" value="1" aria-label="Page number";}button type="submit"{"Go"}}
            }
            section{h2{"Combine into a video"}p{"Select media above, then arrange the clips in order. Original clip audio is preserved; gallery previews are muted."}ol #selection{}label{"Seconds per still image " input #still-seconds type="number" min="0.5" max="60" step="0.5" value="3";}button #combine type="button"{"Combine selected"}}
            section #face-swap{h2{"Face swap"}p{"Choose a source image and target image or video using the labelled buttons on their gallery cards."}
                p #source-label{"Source: none"}p #target-label{"Target: none"}
                h3{"1. Box the source face"}p{"Drag a box around the face to use. The source must be an image."}canvas #source-picker aria-label="Draw source face box"{}button #reset-source type="button"{"Reset source box"}
                h3{"2. Pick the target face"}button #scan-target type="button"{"Find target faces"}p #scan-status role="status"{}canvas #target-picker aria-label="Tap the target face to replace"{}p #chosen-face role="status"{"No target face selected."}
                h3{"3. Save the swap"}button #swap type="button"{"Swap selected face"}
            }
            section #media-jobs aria-live="polite"{}
        }
        dialog #media-viewer aria-label="Full screen media viewer"{button #close-viewer type="button" aria-label="Close full screen viewer"{"Close ✕"}div #viewer-content{}}
        script src=(asset_url("/assets/media.js", include_str!("../../assets/media.js"))){}
    })
}
fn json(request: Request, value: serde_json::Value, status: u16) -> Result<(), String> {
    respond(
        request,
        Response::from_string(value.to_string())
            .with_status_code(status)
            .with_header(header("Content-Type", "application/json")),
    )
}
pub(super) fn form(request: &mut Request) -> Result<Vec<Part>, String> {
    let content = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Content-Type"))
        .map(|h| h.value.as_str().to_owned())
        .unwrap_or_default();
    let boundary = content
        .split("boundary=")
        .nth(1)
        .ok_or("expected multipart form")?
        .trim_matches('"');
    let mut bytes = vec![];
    request
        .as_reader()
        .take(MAX_UPLOAD_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_UPLOAD_BYTES {
        return Err("request exceeds the 160 MB upload limit".into());
    }
    Ok(parse_multipart(&bytes, boundary))
}
pub(super) fn field(parts: &[Part], name: &str) -> Result<String, String> {
    parts
        .iter()
        .find(|p| p.name == name)
        .map(|p| String::from_utf8_lossy(&p.data).into_owned())
        .ok_or_else(|| format!("missing {name}"))
}
pub(super) fn render_job(events: &[Event]) -> Markup {
    let progress = events.iter().rev().find_map(|e| match e {
        Event::Line(Kind::Progress, t) => Some(t),
        _ => None,
    });
    html! {
        @if let Some(progress)=progress{p role="status"{(progress)}}
        @for event in events {
            @match event {
                Event::Video(path)=>figure.image{video src=(file_url(path)) controls muted autoplay loop playsinline preload="metadata"{}},
                other=>(super::web::render_narration(std::slice::from_ref(other))),
            }
        }
        @if events.iter().any(|e|matches!(e,Event::Image(_)|Event::Video(_))){a href="/media"{"View in gallery"}}
    }
}

fn job_markup(t: &RpgTurn) -> Markup {
    html! {article.media-job{h3{(t.action)}div hx-ext="sse" sse-connect=(format!("/media/stream/{}",t.id)) sse-swap="reply" hx-swap="innerHTML" sse-close="done"{p{"Working…"}}}}
}
fn job(
    app: &App,
    request: Request,
    action: &str,
    run: impl FnOnce(&mut Context) -> Result<String, String>,
) -> Result<(), String> {
    let Some(t) = start_turn(app, action.into()) else {
        return json(
            request,
            serde_json::json!({"error":"Another turn or media job is running."}),
            409,
        );
    };
    if let Err(e) = respond(request, html_response(job_markup(&t))) {
        finish(app, &t);
        return Err(e);
    }
    let result = app.owner.with_context(run);
    if let Err(e) = result {
        t.feed.push(Event::Line(Kind::Error, e));
    }
    finish(app, &t);
    Ok(())
}
pub(super) fn handle(app: &App, mut request: Request, path: &str) -> Result<(), String> {
    if request.method() == &Method::Get {
        return match path {
            "/media" => respond(request, html_response(page())),
            "/media/list" => {
                let result = media::catalog(&crate::project::Project {
                    root: app.root.clone(),
                });
                match result {
                    Ok(files) => json(request, serde_json::json!({"files":files}), 200),
                    Err(e) => json(request, serde_json::json!({"error":e}), 400),
                }
            }
            p if p.starts_with("/media/stream/") => {
                let turn = p[14..]
                    .parse::<usize>()
                    .ok()
                    .and_then(|id| app.turns.lock().expect("turns lock").get(id).cloned());
                match turn {
                    Some(t) => stream_feed(request, &t.feed, &render_job),
                    None => respond(
                        request,
                        Response::from_string("no such job").with_status_code(404),
                    ),
                }
            }
            _ => respond(
                request,
                Response::from_string("not found").with_status_code(404),
            ),
        };
    }
    if request.method() != &Method::Post {
        return respond(
            request,
            Response::from_string("method not allowed").with_status_code(405),
        );
    }
    let parts = match form(&mut request) {
        Ok(p) => p,
        Err(e) => return json(request, serde_json::json!({"error":e}), 400),
    };
    match path {
        "/media/upload" => {
            let project = crate::project::Project {
                root: app.root.clone(),
            };
            let result = parts
                .iter()
                .filter(|p| !p.data.is_empty())
                .map(|p| media::upload(&project, &p.name, &p.data))
                .collect::<Result<Vec<_>, _>>();
            match result {
                Ok(paths) => json(request, serde_json::json!({"paths":paths}), 200),
                Err(e) => json(request, serde_json::json!({"error":e}), 400),
            }
        }
        "/media/combine" => {
            let paths = parts
                .iter()
                .filter(|p| p.name == "path")
                .map(|p| String::from_utf8_lossy(&p.data).into_owned())
                .collect::<Vec<_>>();
            let seconds = match field(&parts, "seconds").and_then(|s| {
                s.parse::<f64>()
                    .map_err(|_| "invalid still duration".into())
            }) {
                Ok(n) => n,
                Err(e) => return json(request, serde_json::json!({"error":e}), 400),
            };
            job(app, request, "Combine media", |c| {
                (app.media.combine)(c, &paths, seconds)
            })
        }
        "/media/scan" => {
            let target = match field(&parts, "target") {
                Ok(t) => t,
                Err(e) => return json(request, serde_json::json!({"error":e}), 400),
            };
            let Some(t) = start_turn(app, "Find target faces".into()) else {
                return json(
                    request,
                    serde_json::json!({"error":"Another turn or media job is running."}),
                    409,
                );
            };
            let result = app
                .owner
                .with_context(|context| (app.media.scan)(context, &target));
            finish(app, &t);
            match result {
                Ok(scan) => json(request, serde_json::json!(scan), 200),
                Err(e) => json(request, serde_json::json!({"error":e}), 400),
            }
        }
        "/media/swap" => {
            let args = (|| {
                let source = field(&parts, "source")?;
                let scan = field(&parts, "scan")?;
                let rect = serde_json::from_str::<FaceBox>(&field(&parts, "rect")?)
                    .map_err(|e| e.to_string())?;
                let face = field(&parts, "face")?
                    .parse::<usize>()
                    .map_err(|_| "select a target face")?;
                Ok::<_, String>((source, scan, rect, face))
            })();
            let (source, scan, rect, face) = match args {
                Ok(a) => a,
                Err(e) => return json(request, serde_json::json!({"error":e}), 400),
            };
            job(app, request, "Face swap", |c| {
                (app.media.swap)(c, &source, &scan, &rect, face)
            })
        }
        _ => respond(
            request,
            Response::from_string("not found").with_status_code(404),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpg::test_support::{test_roles, test_session};
    use std::sync::{Arc, Mutex};
    fn fake_scan(c: &mut Context, target: &str) -> Result<Scan, String> {
        assert_eq!(target, ".tmp/clip.webm");
        let path = c.project.root.join(".tmp/preview.png");
        image::RgbImage::from_pixel(200, 100, image::Rgb([80u8, 100, 120]))
            .save(&path)
            .map_err(|e| e.to_string())?;
        Ok(Scan {
            id: "test-scan".into(),
            target: target.into(),
            preview: ".tmp/preview.png".into(),
            frame: 0,
            width: 200,
            height: 100,
            faces: vec![[10.0, 10.0, 70.0, 80.0], [110.0, 10.0, 180.0, 80.0]],
            video: true,
        })
    }
    fn fake_swap(
        c: &mut Context,
        source: &str,
        scan: &str,
        rect: &FaceBox,
        face: usize,
    ) -> Result<String, String> {
        assert_eq!(source, ".tmp/image-00.png");
        assert_eq!(scan, "test-scan");
        assert_eq!(face, 1);
        assert!((0.35..0.55).contains(&rect.width));
        assert!((0.35..0.55).contains(&rect.height));
        let saved = ".tmp/phone-swapped.png";
        std::fs::copy(c.project.root.join(source), c.project.root.join(saved))
            .map_err(|e| e.to_string())?;
        c.emit(Event::Image(saved.into()));
        Ok(saved.into())
    }
    #[test]
    fn media_job_rendering_keeps_progress_and_errors() {
        let rendered = render_job(&[
            Event::Line(Kind::Progress, "first".into()),
            Event::Line(Kind::Progress, "second".into()),
            Event::Line(Kind::Error, "failed".into()),
        ])
        .into_string();
        assert!(rendered.contains("second"));
        assert!(!rendered.contains("first"));
        assert!(rendered.contains("Something went wrong: failed"));
    }
    #[test]
    #[ignore = "requires Playwright/Chromium and ffmpeg; see tests/phone_media.cjs"]
    fn phone_media_browser_integration() {
        let session = Arc::new(Mutex::new(test_session("phone-browser", test_roles())));
        let root = session.lock().unwrap().context.project.root.clone();
        std::fs::create_dir_all(root.join(".tmp")).unwrap();
        for i in 0..14 {
            image::RgbImage::from_pixel(100, 80, image::Rgb([i as u8 * 10, 40, 80]))
                .save(root.join(format!(".tmp/image-{i:02}.png")))
                .unwrap();
        }
        for (ext, codec) in [("mp4", "libx264"), ("webm", "libvpx-vp9")] {
            let result = std::process::Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "color=c=blue:s=160x90:r=30:d=0.5",
                    "-c:v",
                    codec,
                ])
                .arg(root.join(format!(".tmp/clip.{ext}")))
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
        let app = Arc::new(App {
            owner: super::super::web::Owner::Rpg(Arc::downgrade(&session)),
            current: session.lock().unwrap().current.clone(),
            turns: Mutex::default(),
            token: "browser-test".into(),
            root: root.clone(),
            make_image: |_, _, _| Err("unused".into()),
            make_video: |_, _, _, _| Err("unused".into()),
            base: "",
            roleplay: Some(Arc::default()),
            media: Runners {
                combine: media::combine,
                scan: fake_scan,
                swap: fake_swap,
            },
        });
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let url = format!("http://{}/?t=browser-test", server.server_addr());
        let listener = server.clone();
        let worker = std::thread::spawn(move || {
            for request in listener.incoming_requests() {
                let app = app.clone();
                std::thread::spawn(move || super::super::web::handle(&app, request));
            }
        });
        let result = std::process::Command::new("node")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/phone_media.cjs"
            ))
            .arg(&url)
            .arg(&root)
            .output()
            .unwrap();
        server.unblock();
        worker.join().unwrap();
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
    #[test]
    fn media_page_uses_actual_form_controls() {
        let markup = page().into_string();
        assert!(markup.contains("<select id=\"media-filter\""));
        assert!(markup.contains("<input id=\"media-upload\""));
        assert!(markup.contains("<input id=\"still-seconds\""));
    }
}

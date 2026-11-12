//! Phone interface for RPG narration and scene media.
use super::{
    Current, Session, backend::DecisionQuery, scene::scene_prompt, scenes::get_scene_context,
};
use crate::{
    events::{Event, Kind},
    ops::Ops,
    web_util::*,
    workflow::{self, Context},
};
use maud::{Markup, html};
use std::{
    io::Read,
    path::PathBuf,
    sync::{Arc, Mutex, Weak},
    thread::JoinHandle,
};
use tiny_http::{Method, Request, Response, Server};
/// Scene image runner receiving an output path and assembled prompt.
pub type MakeImage = fn(&mut Context, &str, &str) -> Result<String, String>;
/// Scene video runner receiving an output path, optional keyframe and prompt.
pub type MakeVideo = fn(&mut Context, &str, Option<&str>, &str) -> Result<String, String>;
pub(super) struct RpgTurn {
    pub(super) id: usize,
    pub(super) action: String,
    pub(super) feed: Arc<Feed>,
}
pub(super) enum Owner {
    Rpg(Weak<Mutex<Session>>),
    Chat(Weak<Mutex<crate::messages::Conversation>>),
}
impl Owner {
    pub(super) fn rpg(&self) -> Option<Arc<Mutex<Session>>> {
        match self {
            Self::Rpg(s) => s.upgrade(),
            Self::Chat(_) => None,
        }
    }
    pub(super) fn with_context<T>(
        &self,
        run: impl FnOnce(&mut Context) -> Result<T, String>,
    ) -> Result<T, String> {
        match self {
            Self::Rpg(s) => {
                let s = s.upgrade().ok_or("the session has ended")?;
                run(&mut s.lock().expect("session lock").context)
            }
            Self::Chat(c) => {
                let c = c.upgrade().ok_or("the conversation has ended")?;
                run(&mut c.lock().expect("conversation lock").context)
            }
        }
    }
}
/// The same media tools attached to an ordinary chat conversation.
pub(crate) fn media_for_chat(
    conversation: Weak<Mutex<crate::messages::Conversation>>,
    current: Current,
    root: &std::path::Path,
) -> App {
    App {
        owner: Owner::Chat(conversation),
        current,
        turns: Mutex::default(),
        token: String::new(),
        root: root.to_owned(),
        make_image: default_make_image,
        make_video: default_make_video,
        media: Default::default(),
        base: "",
        roleplay: Some(Arc::default()),
    }
}
/// Shared phone navigation for chat and RPG pages.
pub(crate) fn media_menu() -> Markup {
    super::media_web::menu()
}
/// Handles media routes after the host page has checked authentication.
pub(crate) fn handle_media(app: &App, request: Request, path: &str) -> Result<(), String> {
    super::media_web::handle(app, request, path)
}
/// Opens phone roleplay options and session routes after host authentication.
pub(crate) fn handle_roleplay(
    host: &App,
    request: Request,
    path: &str,
    token: &str,
) -> Result<(), String> {
    let hub = host
        .roleplay
        .as_ref()
        .ok_or("roleplay options are unavailable here")?;
    super::roleplay_web::handle(hub, host, request, path, token)
}
pub(super) fn hosted_session(session: &Arc<Mutex<Session>>, token: &str) -> App {
    let s = session.lock().expect("session lock");
    App {
        owner: Owner::Rpg(Arc::downgrade(session)),
        current: s.current.clone(),
        turns: Mutex::default(),
        token: token.into(),
        root: s.context.project.root.clone(),
        make_image: default_make_image,
        make_video: default_make_video,
        media: Default::default(),
        base: "/roleplay",
        roleplay: None,
    }
}
/// The authenticated phone app, holding only a weak reference to its session.
pub struct App {
    pub(super) owner: Owner,
    pub(super) current: Current,
    pub(super) turns: Mutex<Vec<Arc<RpgTurn>>>,
    pub(super) token: String,
    pub(super) root: PathBuf,
    pub(super) make_image: MakeImage,
    pub(super) make_video: MakeVideo,
    pub(super) media: super::media_web::Runners,
    pub(super) base: &'static str,
    pub(super) roleplay: Option<Arc<super::roleplay_web::Hub>>,
}
/// An active listener that stops accepting when dropped.
pub struct RpgWeb {
    server: Arc<Server>,
    accept: Option<JoinHandle<()>>,
    port: u16,
}
impl RpgWeb {
    /// Takes the listener thread for a standalone server to join.
    pub fn take_accept_thread(&mut self) -> Option<JoinHandle<()>> {
        self.accept.take()
    }
    /// The bound TCP port.
    pub fn port(&self) -> u16 {
        self.port
    }
}
impl Drop for RpgWeb {
    fn drop(&mut self) {
        self.server.unblock();
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}
/// Starts a phone UI and returns its listener handle and QR text.
pub fn start(session: &Arc<Mutex<Session>>, port: u16) -> Result<(RpgWeb, String), String> {
    let server = Arc::new(
        Server::http(("0.0.0.0", port))
            .map_err(|e| format!("could not listen on port {port}: {e}"))?,
    );
    let port = server
        .server_addr()
        .to_ip()
        .ok_or("listener has no TCP address")?
        .port();
    let token = uuid::Uuid::new_v4().simple().to_string();
    let lan = lan_ip().map(|ip| format!("http://{ip}:{port}/?t={token}"));
    let local = format!("http://localhost:{port}/?t={token}");
    let s = session.lock().expect("session lock");
    let app = Arc::new(App {
        owner: Owner::Rpg(Arc::downgrade(session)),
        current: s.current.clone(),
        turns: Mutex::default(),
        token,
        root: s.context.project.root.clone(),
        make_image: default_make_image,
        make_video: default_make_video,
        media: Default::default(),
        base: "",
        roleplay: Some(Arc::default()),
    });
    drop(s);
    let listener = server.clone();
    let accept = std::thread::spawn(move || {
        for request in listener.incoming_requests() {
            let app = app.clone();
            std::thread::spawn(move || handle(&app, request));
        }
    });
    let mut message = String::new();
    if let Some(url) = lan {
        message += &qr_code(&url)?;
        message += &format!("\nScan to open the RPG session: {url}\n");
    }
    message += &format!(
        "On this machine: {local}\nAnyone with the link can use this session; stop it with /serve stop."
    );
    Ok((
        RpgWeb {
            server,
            accept: Some(accept),
            port,
        },
        message,
    ))
}
pub(super) fn handle(app: &App, request: Request) {
    let url = request.url().to_owned();
    let (raw_path, query) = url.split_once('?').unwrap_or((&url, ""));
    let path = raw_path.strip_prefix(app.base).unwrap_or(raw_path);
    let method = request.method().clone();
    let result=match (&method,path){
        (Method::Get,"/assets/htmx.js")=>respond(request,asset(HTMX,"text/javascript")),
        (Method::Get,"/assets/sse.js")=>respond(request,asset(HTMX_SSE,"text/javascript")),
        (Method::Get,"/assets/style.css")=>respond(request,asset(STYLE,"text/css")),
        (Method::Get,"/assets/media.js")=>respond(request,asset(include_str!("../../assets/media.js"),"text/javascript")),
        (Method::Get,"/assets/media.css")=>respond(request,asset(include_str!("../../assets/media.css"),"text/css")),
        (Method::Get,"/assets/rpg.js")=>respond(request,asset(include_str!("../../assets/rpg.js"),"text/javascript")),
        (Method::Get,"/") if query_param(query,"t")==Some(app.token.as_str())=>{let cookie=format!("{COOKIE}={}; Path=/; HttpOnly; SameSite=Strict",app.token);respond(request,Response::from_string("").with_status_code(303).with_header(header("Location","/")).with_header(header("Set-Cookie",&cookie)))},
        _ if !authorized(&app.token,&request)=>respond(request,html_response(page_shell(html!{main.locked{p{"Scan the QR code in the terminal running " code{"forgeflow rpg serve"}" to open this session."}}})).with_status_code(403)),
        _ if app.roleplay.is_some() && (path=="/roleplay" || path.starts_with("/roleplay/"))=>handle_roleplay(app,request,path,&app.token),
        _ if path=="/media" || path.starts_with("/media/")=>super::media_web::handle(app,request,path),
        (Method::Get,"/"|"/play")=>respond(request,html_response(page(app))),
        (Method::Post,"/send")=>send(app,request),
        (Method::Get,p) if p.starts_with("/files/")=>serve_file(&app.root,request,&p[7..]),
        (Method::Get,p) if p.starts_with("/stream/")=>{let turn=p[8..].parse::<usize>().ok().and_then(|id|app.turns.lock().expect("turns lock").get(id).cloned());match turn{Some(t)=>stream_feed(request,&t.feed,&render_narration),None=>respond(request,Response::from_string("no such turn").with_status_code(404))}},
        (Method::Post,p @ ("/scene/image"|"/scene/video"))=>{let video=p.ends_with("video");match start_scene(app,video){None=>respond(request,html_response(notice("Still working on the last turn."))),Some(t)=>respond(request,html_response(turn_markup_for(app,&t))).map(|()|run_scene(app,&t,video))}},
        _=>respond(request,Response::from_string("not found").with_status_code(404)),
    };
    if let Err(e) = result {
        eprintln!("rpg web: {method} {path}: {e}");
    }
}
fn send(app: &App, mut request: Request) -> Result<(), String> {
    let content_type = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Content-Type"))
        .map(|h| h.value.as_str().to_owned())
        .unwrap_or_default();
    let mut body = vec![];
    request
        .as_reader()
        .take(MAX_UPLOAD_BYTES as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|e| e.to_string())?;
    if body.len() > MAX_UPLOAD_BYTES {
        return respond(request, html_response(notice("That request is too large.")));
    }
    let Some(boundary) = content_type
        .split("boundary=")
        .nth(1)
        .map(|s| s.trim_matches('"'))
    else {
        return respond(request, html_response(notice("Expected a multipart form.")));
    };
    let action = parse_multipart(&body, boundary)
        .into_iter()
        .find(|p| p.name == "action")
        .map(|p| String::from_utf8_lossy(&p.data).trim().to_owned())
        .unwrap_or_default();
    if action.is_empty() {
        return respond(request, html_response(html! {}));
    }
    let Some(t) = start_turn(app, action) else {
        return respond(
            request,
            html_response(notice("Still working on the last turn.")),
        );
    };
    respond(request, html_response(turn_markup_for(app, &t)))?;
    run_turn_in(app, &t);
    Ok(())
}
fn page(app: &App) -> Markup {
    let (name, chapter, turn, recap) = app
        .owner
        .rpg()
        .map(|s| {
            let s = s.lock().expect("session lock");
            let mut m = s.store.meta().clone();
            let w = super::world::World {
                db: s.store.db(),
                meta: &mut m,
                changes: vec![],
            };
            (
                w.meta.name.clone(),
                w.meta
                    .current_scene_id
                    .and_then(|id| w.scene(id).ok())
                    .map(|s| s.chapter.to_string())
                    .unwrap_or_else(|| "-".into()),
                w.meta.turn,
                super::ui::resume_description(&s.store)
                    .unwrap_or_else(|error| format!("Could not load session recap: {error}")),
            )
        })
        .unwrap_or_else(|| ("Session ended".into(), "-".into(), 0, String::new()));
    let turns = app.turns.lock().expect("turns lock").clone();
    let has_story =
        turns.iter().any(|turn| {
            turn.feed.snapshot().events.iter().any(
                |event| matches!(event, Event::Text(Kind::Chat, text) if !text.trim().is_empty()),
            )
        });
    page_shell(html! {
        link rel="stylesheet" href=(asset_url("/assets/media.css", include_str!("../../assets/media.css")));
        header.bar{span.brand{(name)}span.model{(format!("ch {chapter} · turn {turn}"))}(super::media_web::menu())}
        main #log{
            @if !has_story && !recap.is_empty() {
                div.msg.bot { div.md.k-chat { (maud::PreEscaped(markdown(&recap))) } }
            }
            @for t in &turns{(turn_markup_for(app,t))}
        }
        form #composer hx-post=(format!("{}/send",app.base)) hx-encoding="multipart/form-data" hx-target="#log" hx-swap="beforeend"{
            button type="button" hx-post=(format!("{}/scene/image",app.base)) hx-target="#log" hx-swap="beforeend"{"🖼 Scene"}
            button type="button" hx-post=(format!("{}/scene/video",app.base)) hx-target="#log" hx-swap="beforeend"{"🎬 Scene"}
            div.field{textarea #message name="action" rows="1" placeholder="What do you do?" enterkeyhint="send"{}}
            button.send type="submit"{"↑"}
        }script src="/assets/rpg.js"{}
    })
}
fn turn_markup_for(app: &App, t: &RpgTurn) -> Markup {
    if app.base.is_empty() {
        turn_markup(t)
    } else {
        turn_markup_at(t, app.base)
    }
}
fn turn_markup(t: &RpgTurn) -> Markup {
    turn_markup_at(t, "")
}
fn turn_markup_at(t: &RpgTurn, base: &str) -> Markup {
    let state = t.feed.snapshot();
    html! {div.turn{div.msg.user{div.text{(t.action)}}@if state.done{div.msg.bot{(render_narration(&state.events))}}@else{div.msg.bot hx-ext="sse" sse-connect=(format!("{base}/stream/{}",t.id)) sse-swap="reply" hx-swap="innerHTML" sse-close="done"{span.typing{"…"}}}}}
}
/// Renders story prose, scene media, and visible failures, omitting GM output.
pub fn render_narration(events: &[Event]) -> Markup {
    let mut blocks = vec![];
    let mut text = String::new();
    let flush = |blocks: &mut Vec<Markup>, text: &mut String| {
        if !text.trim().is_empty() {
            blocks.push(html! {div.md.k-chat{(maud::PreEscaped(markdown(text)))}});
        }
        text.clear();
    };
    for event in events {
        match event {
            Event::Text(Kind::Chat, t) => text += t,
            Event::Image(p) => {
                flush(&mut blocks, &mut text);
                blocks.push(html!{figure.image{a href=(file_url(p)){img src=(file_url(p)) alt="scene" loading="lazy";}}});
            }
            Event::Video(p) => {
                flush(&mut blocks, &mut text);
                blocks.push(html!{figure.image{video src=(file_url(p)) controls playsinline preload="metadata"{}}});
            }
            Event::Line(Kind::Error, t) => {
                flush(&mut blocks, &mut text);
                blocks.push(html! {div.line.k-error{"Something went wrong: "(t)}});
            }
            _ => {}
        }
    }
    flush(&mut blocks, &mut text);
    html! {@for block in blocks{(block)}}
}
pub(super) fn start_turn(app: &App, action: String) -> Option<Arc<RpgTurn>> {
    let mut current = app.current.lock().expect("current lock");
    if current.is_some() {
        return None;
    }
    let mut turns = app.turns.lock().expect("turns lock");
    let turn = Arc::new(RpgTurn {
        id: turns.len(),
        action,
        feed: Arc::default(),
    });
    turns.push(turn.clone());
    *current = Some(turn.feed.clone());
    Some(turn)
}
pub(super) fn finish(app: &App, t: &RpgTurn) {
    *app.current.lock().expect("current lock") = None;
    t.feed.finish();
}
fn run_turn_in(app: &App, t: &RpgTurn) {
    if let Some(session) = app.owner.rpg() {
        let mut s = session.lock().expect("session lock");
        if let Err(e) = super::turn::run_turn(&mut s, &t.action) {
            s.context.emit(Event::Line(Kind::Error, e));
        }
    } else {
        t.feed
            .push(Event::Line(Kind::Error, "the session has ended".into()));
    }
    finish(app, t);
}
fn start_scene(app: &App, video: bool) -> Option<Arc<RpgTurn>> {
    start_turn(app, if video { "🎬 Scene" } else { "🖼 Scene" }.into())
}
fn run_scene(app: &App, t: &RpgTurn, video: bool) {
    let keyframe = app
        .turns
        .lock()
        .expect("turns lock")
        .iter()
        .rev()
        .filter(|p| p.id < t.id)
        .find_map(|p| {
            p.feed.snapshot().events.into_iter().rev().find_map(|e| {
                if let Event::Image(p) = e {
                    Some(p)
                } else {
                    None
                }
            })
        });
    let result = (|| {
        let session = app.owner.rpg().ok_or("the session has ended")?;
        let mut s = session.lock().expect("session lock");
        let ctx = get_scene_context(
            s.store.db(),
            s.store.meta(),
            s.store.meta().current_scene_id,
            vec![],
        )?;
        let Session { context, roles, .. } = &mut *s;
        let tone = roles
            .compel_decider
            .decide(
                context,
                &DecisionQuery::Tone {
                    scene_id: ctx.scene.as_ref().map(|s| s.id),
                    scene_text: ctx.scene_text(),
                },
            )
            .ok()
            .map(|a| a.yes);
        let prompt = scene_prompt(&ctx, tone);
        if video {
            let path = next_path(&app.root, "rpg-scene", "mp4");
            (app.make_video)(context, &path, keyframe.as_deref(), &prompt)?;
        } else {
            let path = next_path(&app.root, "rpg-scene", "png");
            (app.make_image)(context, &path, &prompt)?;
        }
        Ok::<(), String>(())
    })();
    if let Err(e) = result {
        t.feed.push(Event::Line(Kind::Error, e));
    }
    finish(app, t);
}
fn default_make_image(c: &mut Context, path: &str, prompt: &str) -> Result<String, String> {
    workflow::run_steps(
        c,
        &[
            Ops::StringLiteral(path.into()),
            Ops::StringLiteral(prompt.into()),
            Ops::MakeImage,
        ],
    )?;
    c.stack
        .pop()
        .map(|v| v.to_text())
        .ok_or("make-image returned no path".into())
}
fn default_make_video(
    c: &mut Context,
    path: &str,
    keyframe: Option<&str>,
    prompt: &str,
) -> Result<String, String> {
    let mut steps = vec![Ops::StringLiteral(path.into())];
    if let Some(k) = keyframe {
        steps.push(Ops::StringLiteral(k.into()));
    }
    steps.push(Ops::StringLiteral(prompt.into()));
    steps.push(if keyframe.is_some() {
        Ops::MakeVideo
    } else {
        Ops::DraftVideo
    });
    workflow::run_steps(c, &steps)?;
    c.stack
        .pop()
        .map(|v| v.to_text())
        .ok_or("make-video returned no path".into())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpg::test_support::*;
    fn image(c: &mut Context, path: &str, _: &str) -> Result<String, String> {
        std::fs::create_dir_all(c.project.root.join(".tmp")).unwrap();
        std::fs::write(c.project.root.join(path), b"").unwrap();
        c.emit(Event::Image(path.into()));
        Ok(path.into())
    }
    fn video(
        c: &mut Context,
        path: &str,
        keyframe: Option<&str>,
        _: &str,
    ) -> Result<String, String> {
        c.emit(Event::Line(
            Kind::Note,
            format!("keyframe {}", keyframe.unwrap_or("none")),
        ));
        c.emit(Event::Video(path.into()));
        Ok(path.into())
    }
    fn app(name: &str) -> (App, Arc<Mutex<Session>>) {
        let s = Arc::new(Mutex::new(test_session(name, test_roles())));
        let guard = s.lock().unwrap();
        let app = App {
            owner: Owner::Rpg(Arc::downgrade(&s)),
            current: guard.current.clone(),
            turns: Mutex::default(),
            token: "abc".into(),
            root: guard.context.project.root.clone(),
            make_image: image,
            make_video: video,
            media: Default::default(),
            base: "",
            roleplay: Some(Arc::default()),
        };
        drop(guard);
        (app, s)
    }
    fn cleanup(a: App, s: Arc<Mutex<Session>>) {
        let root = a.root.clone();
        drop(a);
        drop(s);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn render_narration_shows_only_the_story() {
        let text = render_narration(&[
            Event::Text(Kind::Chat, "The bridge groans.".into()),
            Event::Line(Kind::Note, "⚙ add-aspect".into()),
            Event::Line(Kind::Run, "x".into()),
            Event::Line(Kind::Progress, "step 1".into()),
            Event::Image(".tmp/a.png".into()),
            Event::Line(Kind::Error, "boom".into()),
        ])
        .into_string();
        for s in [
            "The bridge groans.",
            "/files/.tmp/a.png",
            "Something went wrong: boom",
        ] {
            assert!(text.contains(s));
        }
        for s in ["add-aspect", "step 1", ">x<"] {
            assert!(!text.contains(s));
        }
    }
    #[test]
    fn send_runs_the_rpg_turn_not_a_program() {
        let (a, s) = app("web-send");
        let t = start_turn(&a, "I wait".into()).unwrap();
        assert!(
            turn_markup(&t)
                .into_string()
                .contains("sse-connect=\"/stream/0\"")
        );
        run_turn_in(&a, &t);
        assert_eq!(s.lock().unwrap().store.meta().turn, 1);
        let events = t.feed.snapshot().events;
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Text(Kind::Chat, _)))
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Event::Line(Kind::Run, _)))
        );
        cleanup(a, s);
    }
    #[test]
    fn a_second_send_while_running_is_refused() {
        let (a, s) = app("web-busy");
        start_turn(&a, "wait".into()).unwrap();
        assert!(start_turn(&a, "again".into()).is_none());
        cleanup(a, s);
    }
    #[test]
    fn scene_image_pushes_an_image_event() {
        let (a, s) = app("web-image");
        let t = start_scene(&a, false).unwrap();
        run_scene(&a, &t, false);
        let events = t.feed.snapshot().events;
        assert!(
            events
                .iter()
                .any(|e| matches!(e,Event::Image(p) if p.starts_with(".tmp/rpg-scene-")))
        );
        assert!(
            render_narration(&events)
                .into_string()
                .contains("/files/.tmp/rpg-scene-1.png")
        );
        cleanup(a, s);
    }
    #[test]
    fn scene_video_uses_the_latest_scene_image() {
        let (a, s) = app("web-video");
        let t = start_scene(&a, false).unwrap();
        run_scene(&a, &t, false);
        let t = start_scene(&a, true).unwrap();
        run_scene(&a, &t, true);
        assert!(t.feed.snapshot().events.contains(&Event::Line(
            Kind::Note,
            "keyframe .tmp/rpg-scene-1.png".into()
        )));
        cleanup(a, s);
    }
    #[test]
    fn the_page_has_no_chat_controls() {
        let (a, s) = app("web-page");
        let page = page(&a).into_string();
        for t in ["What do you do?", "🖼 Scene", "🎬 Scene"] {
            assert!(page.contains(t));
        }
        assert!(!page.contains("name=\"mode\""));
        assert!(!page.contains("type=\"file\""));
        cleanup(a, s);
    }
    #[test]
    fn a_reopened_page_shows_the_saved_story_without_running_a_turn() {
        let (a, s) = app("web-resume");
        {
            let mut session = s.lock().unwrap();
            let mut meta = session.store.meta().clone();
            meta.turn = 1;
            session.store.save_meta(meta).unwrap();
            std::fs::write(
                session.store.dir().join("transcripts/turn-1.json"),
                r#"{"action":"I wait","narration":"Gareth lowers his lantern."}"#,
            )
            .unwrap();
        }
        let rendered = page(&a).into_string();
        assert!(rendered.contains("Gareth lowers his lantern."));
        assert!(rendered.contains("Scene: The Bridge"));
        assert_eq!(s.lock().unwrap().store.meta().turn, 1);
        assert!(a.turns.lock().unwrap().is_empty());
        cleanup(a, s);
    }
    #[test]
    fn media_jobs_do_not_hide_the_saved_resume_story() {
        let (a, s) = app("web-resume-media");
        {
            let mut session = s.lock().unwrap();
            let mut meta = session.store.meta().clone();
            meta.turn = 1;
            session.store.save_meta(meta).unwrap();
            std::fs::write(
                session.store.dir().join("transcripts/turn-1.json"),
                r#"{"action":"I wait","narration":"Gareth lowers his lantern."}"#,
            )
            .unwrap();
        }
        let turn = start_turn(&a, "Combine media".into()).unwrap();
        turn.feed.push(Event::Video(".tmp/result.mp4".into()));
        turn.feed.finish();
        assert!(
            page(&a)
                .into_string()
                .contains("Gareth lowers his lantern.")
        );
        cleanup(a, s);
    }
}

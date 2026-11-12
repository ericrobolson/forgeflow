//! Phone session options hosted by the existing chat or RPG listener.
use super::{
    Current, Session,
    backend::Roles,
    media_web::{field, form},
    store::Store,
    web::App,
    world::World,
};
use crate::{project::Project, web_util::*, workflow::Context};
use maud::{Markup, html};
use std::sync::{Arc, Mutex, atomic::AtomicBool};
use tiny_http::{Method, Request, Response};

#[derive(Default)]
pub(super) struct Hub {
    active: Mutex<Option<Arc<Active>>>,
}
struct Active {
    session: Arc<Mutex<Session>>,
    app: App,
}
impl Drop for Active {
    fn drop(&mut self) {
        if let Ok(s) = self.session.lock() {
            if let Err(e) = s.store.checkpoint() {
                eprintln!("roleplay backup: {e}");
            }
        }
    }
}
impl Hub {
    fn active(&self) -> Option<Arc<Active>> {
        self.active.lock().expect("roleplay lock").clone()
    }
}
fn redirect(request: Request, path: &str) -> Result<(), String> {
    respond(
        request,
        Response::from_string("")
            .with_status_code(303)
            .with_header(header("Location", path)),
    )
}
fn original(host: &App) -> Option<Arc<Mutex<Session>>> {
    host.owner.rpg()
}
fn options(hub: &Hub, host: &App, error: Option<&str>) -> Result<Markup, String> {
    let project = Project {
        root: host.root.clone(),
    };
    let sessions = Store::list(&project)?;
    let active = hub.active();
    let current = active
        .as_ref()
        .map(|a| a.session.clone())
        .or_else(|| original(host));
    let current = current.map(|s| {
        let s = s.lock().expect("session lock");
        (
            s.store
                .dir()
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            s.store.meta().name.clone(),
        )
    });
    Ok(page_shell(html! {
        link rel="stylesheet" href=(asset_url("/assets/media.css", include_str!("../../assets/media.css")));
        header.bar{span.brand{"Roleplay"}(super::media_web::menu())}
        main.roleplay-options{
            h1{"Roleplay"}
            @if let Some(error)=error{p.k-error role="alert"{(error)}}
            @if let Some((_,name))=&current{
                section{h2{"Current session: "(name)}
                    a.media-link href=(if active.is_some(){"/roleplay/play"}else{"/"}){"Continue session"}
                    @if active.is_some(){form method="post" action="/roleplay/close" enctype="multipart/form-data"{button type="submit"{"Close session"}}}
                }
            }
            section{h2{"New session"}
                form method="post" action="/roleplay/new" enctype="multipart/form-data"{
                    label{"Name " input name="name" required maxlength="80" placeholder="Crown of Ashes";}
                    label{"World " select name="seed"{option value="sample"{"Sample: Crown of Ashes"}option value="blank"{"Blank world"}}}
                    button type="submit"{"Create session"}
                }
            }
            section{h2{"Resume a session"}
                @if sessions.is_empty(){p{"No saved sessions yet."}}
                ul{@for saved in sessions{
                    li{strong{(saved.meta.name)}" · turn "(saved.meta.turn)" · "(saved.meta.game_time)
                        @if current.as_ref().is_some_and(|(dir,_)|dir==&saved.dir_name){
                            " " a href=(if active.is_some(){"/roleplay/play"}else{"/"}){"Continue"}
                        }@else{
                            form method="post" action="/roleplay/open" enctype="multipart/form-data"{
                                input type="hidden" name="session" value=(saved.dir_name);
                                button type="submit"{"Resume "(saved.meta.name)}
                            }
                        }
                    }
                }}
            }
        }
    }))
}
struct Busy(Current);
impl Drop for Busy {
    fn drop(&mut self) {
        *self.0.lock().expect("current lock") = None;
    }
}
fn reserve(host: &App) -> Result<Busy, String> {
    let mut current = host.current.lock().expect("current lock");
    if current.is_some() {
        return Err("Wait for the current turn or media job to finish.".into());
    }
    *current = Some(Arc::default());
    Ok(Busy(host.current.clone()))
}
fn activate(hub: &Hub, host: &App, store: Store, token: &str) -> Result<(), String> {
    let roles = Roles::load(&store.meta().roles)?;
    let current = host.current.clone();
    let debug = Arc::new(AtomicBool::new(false));
    let context = host.owner.with_context(|source| {
        let mut context = super::session_context(source, current.clone(), debug.clone());
        source.server = context.server.take();
        Ok(context)
    })?;
    let session = Arc::new(Mutex::new(Session {
        context,
        store,
        roles,
        current,
        debug,
    }));
    let app = super::web::hosted_session(&session, token);
    let old = hub
        .active
        .lock()
        .expect("roleplay lock")
        .replace(Arc::new(Active { session, app }));
    drop(old);
    Ok(())
}
struct ServerLoan<'a> {
    source: &'a mut Context,
    session: Arc<Mutex<Session>>,
}
impl Drop for ServerLoan<'_> {
    fn drop(&mut self) {
        let mut session = self.session.lock().unwrap_or_else(|e| e.into_inner());
        self.source.server = session.context.server.take();
    }
}
pub(super) fn handle(
    hub: &Hub,
    host: &App,
    mut request: Request,
    path: &str,
    token: &str,
) -> Result<(), String> {
    if request.method() == &Method::Get && matches!(path, "/roleplay" | "/roleplay/") {
        return respond(request, html_response(options(hub, host, None)?));
    }
    if matches!(path, "/roleplay/new" | "/roleplay/open" | "/roleplay/close") {
        if request.method() != &Method::Post {
            return respond(
                request,
                Response::from_string("method not allowed").with_status_code(405),
            );
        }
        let result = (|| {
            let _busy = reserve(host)?;
            let parts = form(&mut request)?;
            let project = Project {
                root: host.root.clone(),
            };
            match path {
                "/roleplay/close" => {
                    let old = hub.active.lock().expect("roleplay lock").take();
                    drop(old);
                }
                "/roleplay/new" => {
                    let name = field(&parts, "name")?;
                    let seed = field(&parts, "seed")?;
                    if !["sample", "blank"].contains(&seed.as_str()) {
                        return Err("Choose a sample or blank world.".into());
                    }
                    let mut store = Store::create(&project, &name)?;
                    if seed == "sample" {
                        let mut meta = store.meta().clone();
                        super::seed::sample(&mut World {
                            db: store.db(),
                            meta: &mut meta,
                            changes: vec![],
                        })?;
                        store.save_meta(meta)?;
                    }
                    super::echo::write(&store)?;
                    activate(hub, host, store, token)?;
                }
                "/roleplay/open" => {
                    let name = field(&parts, "session")?;
                    if hub.active().is_some_and(|a| {
                        a.session
                            .lock()
                            .expect("session lock")
                            .store
                            .dir()
                            .file_name()
                            .is_some_and(|d| d == name.as_str())
                    }) {
                        return Ok(());
                    }
                    activate(hub, host, Store::open(&project, &name)?, token)?;
                }
                _ => {}
            }
            Ok::<_, String>(())
        })();
        return match result {
            Ok(()) => redirect(
                request,
                if path == "/roleplay/close" {
                    "/roleplay"
                } else {
                    "/roleplay/play"
                },
            ),
            Err(e) => respond(
                request,
                html_response(options(hub, host, Some(&e))?).with_status_code(400),
            ),
        };
    }
    let Some(active) = hub.active() else {
        return redirect(request, "/roleplay");
    };
    if request.method() == &Method::Post {
        if host.current.lock().expect("current lock").is_some() {
            return respond(
                request,
                html_response(notice("Wait for the current turn or media job to finish."))
                    .with_status_code(409),
            );
        }
        host.owner.with_context(|source| {
            active.session.lock().expect("session lock").context.server = source.server.take();
            let _loan = ServerLoan {
                source,
                session: active.session.clone(),
            };
            super::web::handle(&active.app, request);
            Ok(())
        })
    } else {
        super::web::handle(&active.app, request);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpg::test_support::{test_roles, test_session};
    #[test]
    fn options_show_saved_sessions_and_sample_blank_choices() {
        let session = Arc::new(Mutex::new(test_session("phone-options", test_roles())));
        let app = super::super::web::hosted_session(&session, "test");
        let root = app.root.clone();
        let markup = options(&Hub::default(), &app, None).unwrap().into_string();
        assert!(markup.contains("Current session: Test"));
        assert!(markup.contains("Create session"));
        assert!(markup.contains("value=\"sample\""));
        assert!(markup.contains("value=\"blank\""));
        assert!(markup.contains("Resume a session"));
        assert!(markup.contains("href=\"/roleplay\""));
        drop(app);
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn switching_is_refused_while_a_turn_or_media_job_runs() {
        let session = Arc::new(Mutex::new(test_session("phone-options-busy", test_roles())));
        let app = super::super::web::hosted_session(&session, "test");
        let root = app.root.clone();
        let busy = reserve(&app).unwrap();
        assert!(reserve(&app).is_err());
        drop(busy);
        assert!(app.current.lock().unwrap().is_none());
        drop(app);
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }
}

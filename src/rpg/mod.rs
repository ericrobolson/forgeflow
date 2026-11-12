//! RPG mode: a Fate-style world in SQLite, narrated by a local model.
pub mod backend;
pub mod echo;
pub mod media;
mod media_web;
mod roleplay_web;
pub mod scene;
pub mod scenes;
pub mod schema;
pub mod seed;
pub mod store;
pub mod tools;
pub mod turn;
pub mod ui;
pub mod web;
pub mod world;

use crate::workflow::Context;
/// The feed receiving events while a browser turn runs.
pub type Current = std::sync::Arc<std::sync::Mutex<Option<std::sync::Arc<crate::web_util::Feed>>>>;
/// An open RPG session shared by the terminal and browser.
pub struct Session {
    pub context: Context,
    pub store: store::Store,
    pub roles: backend::Roles,
    pub current: Current,
    pub debug: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

use crate::events::{Event, Kind, Sink, Terminal};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
/// Mirrors RPG output to the active feed and the terminal GM view.
pub struct RpgSink {
    pub current: Current,
    pub terminal: Terminal,
    pub debug: Arc<AtomicBool>,
}
impl RpgSink {
    /// Whether this event belongs in the current terminal view.
    pub fn shows_in_terminal(&self, e: &Event) -> bool {
        self.debug.load(Ordering::Relaxed) || !matches!(e, Event::Line(Kind::Note, _))
    }
}
impl Sink for RpgSink {
    fn emit(&mut self, e: Event) {
        if let Some(f) = self.current.lock().expect("current lock").clone() {
            f.push(e.clone());
        }
        if self.shows_in_terminal(&e) {
            self.terminal.emit(e);
        }
    }
}
fn session_context(c: &mut Context, current: Current, debug: Arc<AtomicBool>) -> Context {
    Context {
        stack: vec![],
        provider: c.provider,
        registers: crate::registers::Registers::in_memory(),
        memory: crate::registers::Memory::in_memory(),
        project: crate::project::Project {
            root: c.project.root.clone(),
        },
        server: c.server.take(),
        printed: String::new(),
        store_label: "!".into(),
        user_words: c.user_words.clone(),
        local_frames: vec![],
        call_depth: 0,
        sink: Box::new(RpgSink {
            current,
            terminal: Terminal::default(),
            debug,
        }),
        say_kind: Kind::Run,
        web: None,
    }
}
fn pick_store(
    c: &Context,
    input: &mut dyn std::io::BufRead,
    out: &mut dyn std::io::Write,
) -> Result<Option<store::Store>, String> {
    match ui::pick_session(&c.project, input, out)? {
        ui::Pick::Quit => Ok(None),
        ui::Pick::Open(name) => {
            let store = store::Store::open(&c.project, &name)?;
            writeln!(out, "{}", ui::resume_description(&store)?).map_err(|e| e.to_string())?;
            Ok(Some(store))
        }
        ui::Pick::New { name, sample } => {
            let mut s = store::Store::create(&c.project, &name)?;
            if sample {
                let mut m = s.meta().clone();
                seed::sample(&mut world::World {
                    db: s.db(),
                    meta: &mut m,
                    changes: vec![],
                })?;
                s.save_meta(m)?;
            }
            writeln!(
                out,
                "  created {}/world.db",
                s.dir()
                    .strip_prefix(&c.project.root)
                    .unwrap_or(s.dir())
                    .display()
            )
            .map_err(|e| e.to_string())?;
            if sample {
                writeln!(
                    out,
                    "  sample world loaded: 2 PCs, 3 NPCs, 1 location, opening scene begun"
                )
                .map_err(|e| e.to_string())?;
            }
            Ok(Some(s))
        }
    }
}
fn open_session(c: &mut Context, store: store::Store) -> Result<Arc<Mutex<Session>>, String> {
    let roles = backend::Roles::load(&store.meta().roles)?;
    let current = Arc::default();
    let debug = Arc::new(AtomicBool::new(false));
    let context = session_context(c, Arc::clone(&current), Arc::clone(&debug));
    Ok(Arc::new(Mutex::new(Session {
        context,
        store,
        roles,
        current,
        debug,
    })))
}
/// Picks a session and plays until /back or /quit.
pub fn run(c: &mut Context) -> Result<(), String> {
    use std::io::Write;
    let mut input = std::io::stdin().lock();
    let mut out = std::io::stdout();
    loop {
        let Some(store) = pick_store(c, &mut input, &mut out)? else {
            return Ok(());
        };
        let session = open_session(c, store)?;
        let mut web = match crate::config::Config::load(&c.project.folder())
            .and_then(|config| web::start(&session, config.web_port))
        {
            Ok((web, message)) => {
                writeln!(out, "{message}").map_err(|e| e.to_string())?;
                Some(web)
            }
            Err(error) => {
                writeln!(out, "Could not start the RPG web UI: {error}")
                    .map_err(|e| e.to_string())?;
                None
            }
        };
        loop {
            writeln!(
                out,
                "{}",
                ui::prompt_line(&session.lock().expect("session lock"))?
            )
            .map_err(|e| e.to_string())?;
            write!(out, "> ")
                .and_then(|_| out.flush())
                .map_err(|e| e.to_string())?;
            let mut line = String::new();
            use std::io::BufRead;
            if input.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
                line = "/quit".into();
            }
            match ui::parse_action(&line) {
                ui::Action::Back | ui::Action::Quit => {
                    drop(web.take());
                    let mut s = session.lock().expect("session lock");
                    let result = s.store.checkpoint();
                    c.server = s.context.server.take();
                    result?;
                    if matches!(ui::parse_action(&line), ui::Action::Quit) {
                        return Ok(());
                    }
                    break;
                }
                ui::Action::Turn(text) => {
                    if let Err(e) =
                        turn::run_turn(&mut session.lock().expect("session lock"), &text)
                    {
                        writeln!(out, "error: {e}").map_err(|e| e.to_string())?;
                    }
                }
                ui::Action::Command(text) if text == "/serve" || text.starts_with("/serve ") => {
                    let result = if text == "/serve stop" {
                        drop(web.take());
                        Ok("Stopped the web UI.".into())
                    } else if let Some(w) = &web {
                        Ok(format!(
                            "The web UI is already running on port {}.",
                            w.port()
                        ))
                    } else {
                        let port = if text == "/serve" {
                            crate::config::Config::load(&c.project.folder()).map(|c| c.web_port)
                        } else {
                            text.trim_start_matches("/serve ")
                                .parse::<u16>()
                                .map_err(|_| "invalid web port".into())
                        };
                        port.and_then(|p| web::start(&session, p))
                            .map(|(w, message)| {
                                web = Some(w);
                                message
                            })
                    };
                    writeln!(out, "{}", result.unwrap_or_else(|e| format!("error: {e}")))
                        .map_err(|e| e.to_string())?;
                }
                ui::Action::Command(text) => {
                    let result = ui::run_command(&mut session.lock().expect("session lock"), &text);
                    writeln!(out, "{}", result.unwrap_or_else(|e| format!("error: {e}")))
                        .map_err(|e| e.to_string())?;
                }
            }
        }
    }
}
/// Rewrites a session's events.jsonl from its database.
pub fn rebuild_echo(c: &mut Context, name: &str) -> Result<(), String> {
    let store = store::Store::open(&c.project, name)?;
    echo::write(&store)
}

/// Picks a session and serves it to a phone browser until interrupted.
pub fn serve(c: &mut Context, port: u16) -> Result<(), String> {
    let Some(store) = pick_store(c, &mut std::io::stdin().lock(), &mut std::io::stdout())? else {
        return Ok(());
    };
    let session = open_session(c, store)?;
    let (mut web, message) = web::start(&session, port)?;
    println!("{message}");
    if let Some(accept) = web.take_accept_thread() {
        let _ = accept.join();
    }
    drop(web);
    let mut s = session.lock().expect("session lock");
    let result = s.store.checkpoint();
    c.server = s.context.server.take();
    result
}

#[cfg(test)]
pub(crate) mod test_support {
    use crate::{agent::Provider, project::Project, registers::Registers, workflow::Context};
    pub(crate) fn test_context(name: &str) -> Context {
        let root =
            std::env::temp_dir().join(format!("forgeflow-loop-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("notes.txt"), "the answer is 42").unwrap();
        Context {
            stack: vec![],
            provider: Provider::Codex,
            registers: Registers::in_memory(),
            memory: crate::registers::Memory::in_memory(),
            project: Project { root },
            server: None,
            printed: String::new(),
            store_label: "!".into(),
            user_words: vec![],
            local_frames: vec![],
            call_depth: 0,
            sink: Box::new(crate::events::Recorder::default()),
            say_kind: crate::events::Kind::Run,
            web: None,
        }
    }
    use super::{
        Session,
        backend::{self, Executor, Generation, Roles, Task},
        scenes::{Participant, SceneInput},
        store::Store,
        world::{AspectInput, Scope, World},
    };
    use std::sync::{Arc, Mutex};
    pub(crate) struct ScriptedGen {
        pub steps: Vec<Vec<(String, String)>>,
        pub finals: Vec<String>,
        pub seen: Arc<Mutex<Vec<Task>>>,
    }
    impl Generation for ScriptedGen {
        fn generate(
            &mut self,
            c: &mut Context,
            t: &Task,
            execute: Executor,
            stream: bool,
        ) -> Result<String, String> {
            self.seen.lock().unwrap().push(t.clone());
            if !self.steps.is_empty() {
                for (n, a) in self.steps.remove(0) {
                    let out = execute(c, &n, &a);
                    if out.is_error {
                        return Err(out.content);
                    }
                }
            }
            let text = if self.finals.is_empty() {
                "{\"facts\":[]}".into()
            } else {
                self.finals.remove(0)
            };
            if text == "PANIC" {
                panic!("archive panic");
            }
            if text == "ERROR" {
                return Err("narrator error".into());
            }
            if stream {
                c.emit(crate::events::Event::Text(
                    crate::events::Kind::Chat,
                    text.clone(),
                ));
            }
            Ok(text)
        }
    }
    pub(crate) fn gen_text(text: &str) -> Box<dyn Generation> {
        Box::new(ScriptedGen {
            steps: vec![],
            finals: vec![text.into()],
            seen: Arc::default(),
        })
    }
    pub(crate) fn test_roles() -> Roles {
        Roles {
            narrator: gen_text("The bridge groans."),
            archivist: gen_text("{\"facts\":[]}"),
            compel_decider: Box::new(backend::RulesBackend),
        }
    }
    pub(crate) fn test_session(name: &str, roles: Roles) -> Session {
        let mut context = test_context(name);
        let mut store = Store::create(&context.project, "Test").unwrap();
        let mut m = store.meta().clone();
        {
            let mut w = World {
                db: store.db(),
                meta: &mut m,
                changes: vec![],
            };
            let pc = w.add_entity("Aldric", "character", None).unwrap();
            w.add_aspect(AspectInput {
                text: "Hot-headed".into(),
                scope: Scope::Character,
                holder_id: Some(pc),
                slot: Some("trouble".into()),
                valid_from: None,
            })
            .unwrap();
            let npc = w.add_entity("Gareth", "character", None).unwrap();
            w.begin_scene(SceneInput {
                title: "The Bridge".into(),
                summary: None,
                location_id: None,
                chapter: None,
                participants: vec![
                    Participant {
                        entity_id: pc,
                        role: "pc".into(),
                    },
                    Participant {
                        entity_id: npc,
                        role: "npc".into(),
                    },
                ],
                situation_aspects: vec![],
            })
            .unwrap();
        }
        store.save_meta(m).unwrap();
        let current = Arc::default();
        let debug = Arc::new(std::sync::atomic::AtomicBool::new(true));
        context.sink = Box::new(super::RpgSink {
            current: Arc::clone(&current),
            terminal: crate::events::Terminal::default(),
            debug: Arc::clone(&debug),
        });
        Session {
            context,
            store,
            roles,
            current,
            debug,
        }
    }
}

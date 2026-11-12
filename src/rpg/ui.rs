//! Line-oriented session menu and GM commands.
use super::{
    Session,
    scenes::get_scene_context,
    schema::EntityRow,
    store::Store,
    world::{World, check_game_time, rows},
};
use crate::project::Project;
use rusqlite::OptionalExtension;
use std::{
    io::{BufRead, Write},
    sync::atomic::Ordering,
};
/// A selection from the session menu.
pub enum Pick {
    Open(String),
    New { name: String, sample: bool },
    Quit,
}
/// A player action or terminal navigation command.
pub enum Action {
    Turn(String),
    Command(String),
    Back,
    Quit,
}
fn line(
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    prompt: &str,
) -> Result<Option<String>, String> {
    write!(out, "{prompt}")
        .and_then(|_| out.flush())
        .map_err(|e| e.to_string())?;
    let mut s = String::new();
    if input.read_line(&mut s).map_err(|e| e.to_string())? == 0 {
        Ok(None)
    } else {
        Ok(Some(s.trim().into()))
    }
}
/// Lists session metadata and asks which session to open or create.
pub fn pick_session(
    project: &Project,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<Pick, String> {
    let list = Store::list(project)?;
    writeln!(out, "RPG sessions in {}:", project.root.display()).map_err(|e| e.to_string())?;
    for (i, s) in list.iter().enumerate() {
        writeln!(
            out,
            "  {}  {:24} turn {} · {}",
            i + 1,
            s.dir_name,
            s.meta.turn,
            s.meta.game_time
        )
        .map_err(|e| e.to_string())?;
    }
    writeln!(out, "  n  new session\n  q  quit").map_err(|e| e.to_string())?;
    loop {
        let Some(choice) = line(input, out, "> ")? else {
            return Ok(Pick::Quit);
        };
        match choice.as_str() {
            "q" => return Ok(Pick::Quit),
            "n" => {
                let Some(name) = line(input, out, "Name: ")? else {
                    return Ok(Pick::Quit);
                };
                loop {
                    let Some(seed) = line(input, out, "seed the world? (b)lank, (s)ample\n> ")?
                    else {
                        return Ok(Pick::Quit);
                    };
                    match seed.as_str() {
                        "b" => {
                            return Ok(Pick::New {
                                name,
                                sample: false,
                            });
                        }
                        "s" => return Ok(Pick::New { name, sample: true }),
                        _ => {}
                    }
                }
            }
            _ => {
                if let Some(s) = choice
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| n.checked_sub(1))
                    .and_then(|i| list.get(i))
                {
                    return Ok(Pick::Open(s.dir_name.clone()));
                }
                writeln!(out, "Unknown choice.").map_err(|e| e.to_string())?;
            }
        }
    }
}
/// Separates player actions from terminal commands.
pub fn parse_action(line: &str) -> Action {
    let s = line.trim();
    match s {
        "/back" => Action::Back,
        "/quit" => Action::Quit,
        s if s.starts_with('/') => Action::Command(s.into()),
        s => Action::Turn(s.into()),
    }
}
/// Describes the active scene and committed turn for the prompt.
pub fn prompt_line(s: &Session) -> Result<String, String> {
    let mut m = s.store.meta().clone();
    let w = World {
        db: s.store.db(),
        meta: &mut m,
        changes: vec![],
    };
    let scene = w.meta.current_scene_id.map(|id| w.scene(id)).transpose()?;
    Ok(format!(
        "[{} · ch {} · turn {} · scene: {}]",
        w.meta.name,
        scene
            .as_ref()
            .map(|s| s.chapter.to_string())
            .unwrap_or_else(|| "-".into()),
        w.meta.turn,
        scene.as_ref().map(|s| s.title.as_str()).unwrap_or("none")
    ))
}

/// Replays saved scene facts and the last committed narration without generating or writing anything.
pub fn resume_description(store: &Store) -> Result<String, String> {
    let meta = store.meta();
    let scene_id = match meta.current_scene_id {
        Some(id) => Some(id),
        None => store
            .db()
            .query_row("SELECT id FROM scenes ORDER BY id DESC LIMIT 1", [], |r| {
                r.get(0)
            })
            .optional()
            .map_err(super::schema::sql_error)?,
    };
    let scene = get_scene_context(store.db(), meta, scene_id, vec![])?;
    let mut lines = Vec::new();
    if let Some(current) = &scene.scene {
        lines.push(format!("Scene: {}", current.title));
        if let Some(summary) = &current.summary {
            if !summary.trim().is_empty() {
                lines.push(summary.clone());
            }
        }
        if let Some(location) = &scene.location {
            lines.push(format!("Location: {}", location.name));
        }
        if !scene.present.is_empty() {
            lines.push(format!("Present: {}", scene.present_names().join(", ")));
        }
    } else {
        lines.push("No scene has been established yet.".into());
    }
    if meta.turn > 0 {
        let transcript = std::fs::read_to_string(
            store
                .dir()
                .join(super::store::TRANSCRIPTS)
                .join(format!("turn-{}.json", meta.turn)),
        )
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
        let action = transcript
            .as_ref()
            .and_then(|t| t["action"].as_str())
            .map(str::to_owned);
        let action = match action {
            Some(action) => Some(action),
            None => store.db().query_row("SELECT substr(summary,9) FROM events WHERE turn=?1 AND summary LIKE 'Player: %' ORDER BY id LIMIT 1", [meta.turn], |r| r.get::<_, String>(0))
                .optional().map_err(super::schema::sql_error)?,
        };
        if let Some(action) = action {
            lines.push(format!("\nLast action: {action}"));
        }
        let narration = transcript
            .as_ref()
            .and_then(|t| t["narration"].as_str())
            .filter(|text| !text.trim().is_empty());
        lines.push(format!(
            "\n{}",
            narration.unwrap_or("Last turn's narration is unavailable.")
        ));
    }
    Ok(lines.join("\n"))
}

/// Runs read-only world queries and the terminal GM visibility switch.
pub fn run_command(s: &mut Session, command: &str) -> Result<String, String> {
    if command == "/debug on" || command == "/debug off" {
        let on = command.ends_with(" on");
        s.debug.store(on, Ordering::Relaxed);
        return Ok(format!("GM view {}", if on { "on" } else { "off" }));
    }
    let mut m = s.store.meta().clone();
    let w = World {
        db: s.store.db(),
        meta: &mut m,
        changes: vec![],
    };
    if command == "/scene" {
        return Ok(get_scene_context(w.db, w.meta, w.meta.current_scene_id, vec![])?.render());
    }
    if let Some(name) = command.strip_prefix("/aspects ") {
        let e = match name.parse::<i64>() {
            Ok(id) => w.entity(id)?,
            Err(_) => w
                .find_entity(name)?
                .ok_or_else(|| format!("no entity named {name}"))?,
        };
        return Ok(w
            .aspects(Some(e.id))?
            .iter()
            .map(|a| {
                format!(
                    "#{} {} ({}{})",
                    a.id,
                    a.text,
                    a.scope,
                    a.slot
                        .as_ref()
                        .map(|s| format!(", {s}"))
                        .unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n"));
    }
    if let Some(id) = command.strip_prefix("/history ") {
        return Ok(w
            .aspect_history(id.parse().map_err(|_| "aspect id must be an integer")?)?
            .iter()
            .map(|a| {
                format!(
                    "#{} {} → {}  {}",
                    a.id,
                    a.valid_from,
                    a.valid_to.as_deref().unwrap_or("now"),
                    a.text
                )
            })
            .collect::<Vec<_>>()
            .join("\n"));
    }
    if let Some(time) = command.strip_prefix("/as-of ") {
        check_game_time(time)?;
        let mut lines = vec![];
        for e in rows(
            w.db,
            &format!("SELECT {} FROM entities ORDER BY id", EntityRow::COLUMNS),
            [],
            EntityRow::from_row,
        )? {
            let a = w.aspects_at(Some(e.id), time)?;
            if !a.is_empty() {
                lines.push(format!(
                    "{}: {}",
                    e.name,
                    a.iter()
                        .map(|a| a.text.as_str())
                        .collect::<Vec<_>>()
                        .join("; ")
                ));
            }
        }
        let a = w.aspects_at(None, time)?;
        if !a.is_empty() {
            lines.push(format!(
                "Campaign: {}",
                a.iter()
                    .map(|a| a.text.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        return Ok(lines.join("\n"));
    }
    Err("unknown command; try /scene, /aspects <entity>, /history <aspect id>, /as-of <game time>, /serve [port|stop], /debug on|off, /back, /quit".into())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpg::test_support::*;
    use std::sync::{Arc, atomic::AtomicBool};
    fn cleanup(s: Session) {
        let r = s.context.project.root.clone();
        drop(s);
        std::fs::remove_dir_all(r).unwrap();
    }
    #[test]
    fn pick_session_lists_from_meta_only() {
        let c = test_context("menu");
        let a = Store::create(&c.project, "Alpha").unwrap();
        let dir = a.dir().to_owned();
        drop(a);
        drop(Store::create(&c.project, "Zulu").unwrap());
        std::fs::remove_file(dir.join("world.db")).unwrap();
        let mut out = vec![];
        assert!(matches!(
            pick_session(&c.project, &mut &b"q\n"[..], &mut out).unwrap(),
            Pick::Quit
        ));
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("Alpha"));
        assert!(text.contains("Zulu"));
        assert!(
            matches!(pick_session(&c.project,&mut &b"2\n"[..],&mut vec![]).unwrap(),Pick::Open(s) if s.ends_with("Alpha"))
        );
        std::fs::remove_dir_all(c.project.root).unwrap();
    }
    #[test]
    fn pick_session_creates_with_a_seed_choice() {
        let c = test_context("menu-new");
        let mut out = vec![];
        assert!(
            matches!(pick_session(&c.project,&mut &b"n\nCrown of Ashes\nx\ns\n"[..],&mut out).unwrap(),Pick::New{name,sample:true} if name=="Crown of Ashes")
        );
        assert_eq!(
            String::from_utf8(out)
                .unwrap()
                .matches("seed the world?")
                .count(),
            2
        );
        std::fs::remove_dir_all(c.project.root).unwrap();
    }
    #[test]
    fn as_of_shows_the_past_and_writes_nothing() {
        let mut s = test_session("as-of", test_roles());
        let mut m = s.store.meta().clone();
        {
            let mut w = World {
                db: s.store.db(),
                meta: &mut m,
                changes: vec![],
            };
            w.log_event("1471-03-12T21:00", "later", None).unwrap();
            w.rename_aspect(1, "Cool-headed").unwrap();
        }
        s.store.save_meta(m).unwrap();
        let before = s.store.db().total_changes();
        let text = run_command(&mut s, "/as-of 1471-03-12T20:00").unwrap();
        assert!(text.contains("Hot-headed"));
        assert!(!text.contains("Cool-headed"));
        assert_eq!(s.store.db().total_changes(), before);
        cleanup(s);
    }
    #[test]
    fn aspects_and_history_commands() {
        let mut s = test_session("commands", test_roles());
        assert!(
            run_command(&mut s, "/aspects Aldric")
                .unwrap()
                .contains("Hot-headed")
        );
        assert!(
            run_command(&mut s, "/history 1")
                .unwrap()
                .contains("1471-03-12T20:00 → now")
        );
        cleanup(s);
    }
    #[test]
    fn parse_action_routes_slash_commands() {
        assert!(matches!(parse_action("/back"), Action::Back));
        assert!(matches!(parse_action("/quit"), Action::Quit));
        assert!(matches!(parse_action("/scene"), Action::Command(_)));
        assert!(matches!(parse_action("wait"), Action::Turn(_)));
    }
    #[test]
    fn debug_off_hides_gm_lines_in_the_terminal_sink() {
        use crate::{
            events::{Event, Kind, Sink, Terminal},
            rpg::RpgSink,
        };
        let current = Arc::default();
        let feed = Arc::new(crate::web_util::Feed::default());
        let mut sink = RpgSink {
            current,
            terminal: Terminal::default(),
            debug: Arc::new(AtomicBool::new(false)),
        };
        *sink.current.lock().unwrap() = Some(feed.clone());
        let e = Event::Line(Kind::Note, "gm".into());
        assert!(!sink.shows_in_terminal(&e));
        assert!(sink.shows_in_terminal(&Event::Text(Kind::Chat, "story".into())));
        sink.emit(e);
        assert_eq!(feed.snapshot().events.len(), 1);
    }
    #[test]
    fn resume_description_replays_only_the_committed_turn_without_writes() {
        let mut s = test_session("resume-description", test_roles());
        let mut meta = s.store.meta().clone();
        meta.turn = 1;
        s.store.save_meta(meta).unwrap();
        std::fs::write(s.store.dir().join("transcripts/turn-1.json"),r#"{"action":"I wait","narration":"Gareth lowers his lantern.","changes":["secret bookkeeping"]}"#).unwrap();
        std::fs::write(
            s.store.dir().join("transcripts/turn-2.json"),
            r#"{"action":"future","narration":"Uncommitted story"}"#,
        )
        .unwrap();
        let changes = s.store.db().total_changes();
        let meta = s.store.meta().clone();
        let expected = "Scene: The Bridge\nPresent: Aldric, Gareth\n\nLast action: I wait\n\nGareth lowers his lantern.";
        assert_eq!(resume_description(&s.store).unwrap(), expected);
        assert_eq!(resume_description(&s.store).unwrap(), expected);
        assert_eq!(s.store.db().total_changes(), changes);
        assert_eq!(s.store.meta(), &meta);
        cleanup(s);
    }
    #[test]
    fn resume_description_uses_the_last_scene_when_none_is_active() {
        let mut s = test_session("resume-closed", test_roles());
        let mut meta = s.store.meta().clone();
        World {
            db: s.store.db(),
            meta: &mut meta,
            changes: vec![],
        }
        .end_scene(3, &[])
        .unwrap();
        s.store.save_meta(meta).unwrap();
        assert!(
            resume_description(&s.store)
                .unwrap()
                .starts_with("Scene: The Bridge")
        );
        cleanup(s);
    }
    #[test]
    fn resume_description_handles_missing_and_invalid_transcripts() {
        let mut s = test_session("resume-missing", test_roles());
        let mut meta = s.store.meta().clone();
        meta.turn = 1;
        s.store.save_meta(meta).unwrap();
        s.store.db().execute("INSERT INTO events(turn,game_time,summary) VALUES(1,'1471-03-12T20:00','Player: I listen')",[]).unwrap();
        let text = resume_description(&s.store).unwrap();
        assert!(text.contains("Last action: I listen"));
        assert!(text.contains("Last turn's narration is unavailable."));
        std::fs::write(s.store.dir().join("transcripts/turn-1.json"), "broken JSON").unwrap();
        assert_eq!(resume_description(&s.store).unwrap(), text);
        cleanup(s);
    }
}

//! Atomic turns: compel, narrate, archive, then commit.
use super::{
    Session,
    backend::{self, DecisionAnswer, DecisionQuery, Task},
    echo,
    scenes::get_scene_context,
    schema::sql_error,
    store::{BACKUP_EVERY, TRANSCRIPTS},
    world::{AspectInput, Scope, World},
};
use crate::{
    events::{Event, Kind},
    tools::ToolOutcome,
};

const NARRATION_PACING: &str = "Pacing: Match the scope of the player's request. For a simple question such as 'describe her', give one short paragraph of 2–4 sentences (about 40–80 words): relevant physical features and established personality or observable demeanor. Do not invent private thoughts, add an encounter, advance time, or repeat scenery. Stop when the question is answered. For a normal action, aim for 1–2 short paragraphs, usually under 150 words; expand only when the player asks for detail. Avoid repeated metaphors, grand declarations, and summaries of what you just narrated.
For a full narrative beat, use START internally, without headings or spelling out every stage:
S — Situation: Ground the immediate setting, relevant props, hazards, and people in a few concrete details.
T — Task: Recognize the player's stated goal and any meaningful obstacle. Let guaranteed success happen without a check.
A — Action: Resolve the player's declared action in 1–3 meaningful steps. Do not choose their next actions, thoughts, or feelings for them. Stop at a consequential choice that needs player input.
R — Resolution: Give a concrete outcome; let setbacks introduce a complication or new opportunity instead of stalling. This mode has no dice or oracle engine: never invent rolls, skill-check results, or oracle answers. If uncertainty needs a roll, leave it for the player and narrate only an outcome they supply.
T — Transition: When the beat actually ends, leave one clear opening for the next action or scene. Do not force a scene change or a new hook after a simple description. START is a guide for relevant beats, not a requirement to turn every reply into five stages.";

/// A suggested complication that the narrator may use.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CompelFlag {
    pub aspect_id: i64,
    pub text: String,
    pub answer: DecisionAnswer,
}
/// The committed clock, narration and world changes for one turn.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnReceipt {
    pub turn: i64,
    pub game_time: String,
    pub compels: Vec<CompelFlag>,
    pub changes: Vec<String>,
    pub narration: String,
}
/// A candidate fact extracted by the archivist.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Fact {
    Entity {
        name: String,
        #[serde(rename = "type")]
        entity_type: String,
    },
    Aspect {
        text: String,
        scope: Scope,
        holder_id: Option<i64>,
        slot: Option<String>,
    },
    Relationship {
        from_id: i64,
        to_id: i64,
        #[serde(rename = "type")]
        kind: String,
        score: i64,
    },
}
struct TurnTxn<'a> {
    db: &'a rusqlite::Connection,
    done: bool,
}
impl<'a> TurnTxn<'a> {
    fn begin(db: &'a rusqlite::Connection) -> Result<Self, String> {
        db.execute_batch("BEGIN IMMEDIATE").map_err(sql_error)?;
        Ok(Self { db, done: false })
    }
    fn commit(mut self) -> Result<(), String> {
        self.db.execute_batch("COMMIT").map_err(sql_error)?;
        self.done = true;
        Ok(())
    }
}
impl Drop for TurnTxn<'_> {
    fn drop(&mut self) {
        if !self.done {
            let _ = self.db.execute_batch("ROLLBACK");
        }
    }
}
/// Parses the archivist's JSON, accepting a surrounding Markdown fence.
pub fn parse_facts(text: &str) -> Result<Vec<Fact>, String> {
    let text = text.trim();
    let text = if text.starts_with("```") {
        let (_, body) = text
            .split_once('\n')
            .ok_or("an opening fence needs a newline")?;
        body.strip_suffix("```")
            .ok_or("missing closing fence")?
            .trim()
    } else {
        text
    };
    #[derive(serde::Deserialize)]
    struct Archive {
        facts: Vec<Fact>,
    }
    serde_json::from_str::<Archive>(text)
        .map(|a| a.facts)
        .map_err(|e| e.to_string())
}
/// Plays and commits a complete turn; model errors and panics roll back world changes.
pub fn run_turn(session: &mut Session, action: &str) -> Result<TurnReceipt, String> {
    let action = action.trim();
    if action.is_empty() {
        return Err("say what you do".into());
    }
    let Session {
        context,
        store,
        roles,
        ..
    } = session;
    let mut meta = store.meta().clone();
    let (changes, narration, compels) = {
        let txn = TurnTxn::begin(store.db())?;
        let mut world = World {
            db: store.db(),
            meta: &mut meta,
            changes: vec![],
        };
        world.log_event(
            &world.now().to_owned(),
            &format!("Player: {}", action.chars().take(200).collect::<String>()),
            None,
        )?;
        let mut narrations = vec![];
        for n in world.meta.turn - 1..=world.meta.turn {
            if let Ok(text) = std::fs::read_to_string(
                store.dir().join(TRANSCRIPTS).join(format!("turn-{n}.json")),
            ) {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(text) = json["narration"].as_str() {
                        narrations.push(text.into());
                    }
                }
            }
        }
        let ctx = get_scene_context(
            world.db,
            world.meta,
            world.meta.current_scene_id,
            narrations,
        )?;
        let scene_id = ctx.scene.as_ref().map(|s| s.id);
        let mut compels = vec![];
        for a in ctx.compel_candidates() {
            match roles.compel_decider.decide(
                context,
                &DecisionQuery::Compel {
                    aspect_id: a.id,
                    aspect_text: a.text.clone(),
                    slot: a.slot.clone(),
                    scene_id,
                    scene_text: ctx.scene_text(),
                    action_text: action.into(),
                },
            ) {
                Ok(answer) if answer.yes => {
                    context.emit(Event::Line(
                        Kind::Note,
                        format!(
                            "compel \"{}\" → yes ({}, {:.1})",
                            a.text,
                            roles.compel_decider.name(),
                            answer.confidence
                        ),
                    ));
                    compels.push(CompelFlag {
                        aspect_id: a.id,
                        text: a.text.clone(),
                        answer,
                    });
                }
                Ok(_) => {}
                Err(e) => context.emit(Event::Line(
                    Kind::Note,
                    format!("compel \"{}\" dropped: {e}", a.text),
                )),
            }
        }
        let suggested = if compels.is_empty() {
            String::new()
        } else {
            format!(
                "\n\n## Suggested compels\n{}",
                compels
                    .iter()
                    .map(|f| format!(
                        "- #{} \"{}\": {}",
                        f.aspect_id,
                        f.text,
                        f.answer.reason.as_deref().unwrap_or("")
                    ))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        };
        let flashback = if ctx.flashback {
            format!(
                "\n\nThis is a FLASHBACK to {}. Do not reveal anything that happens later.",
                ctx.as_of
            )
        } else {
            String::new()
        };
        let task = Task {
            system: format!(
                "{}\n\n## Scene\n{}{suggested}{flashback}\n\nCurrent game time: {}. Turn {}.\n\nNarration boundary: Output only the lived story: observable actions, dialogue, surroundings, and feelings. Keep fate points, point spending or awards, relationship types and numeric scores, aspect labels, stress boxes, compels, tool calls, ids, registers, archives, and database bookkeeping entirely out of the prose. Show consequences through events and character behavior; never announce mechanical changes or that the story or world has changed. Do not invent point transactions merely to make a moment feel more intense.\n\n{NARRATION_PACING}",
                backend::prompt(&context.project, "narrator"),
                ctx.render(),
                world.now(),
                world.turn()
            ),
            user: action.into(),
            tools: super::tools::schemas(&context.user_words),
        };
        let narration = {
            let mut execute = |c: &mut crate::workflow::Context, n: &str, a: &str| {
                super::tools::execute(c, &mut world, n, a)
            };
            roles
                .narrator
                .generate(context, &task, &mut execute, true)?
        };
        let archive_user = format!(
            "Narration:\n{narration}\n\nTool calls:\n{}\n\nScene:\n{}",
            world.changes.join("\n"),
            ctx.render()
        );
        let mut archive = Task {
            system: backend::prompt(&context.project, "archivist"),
            user: archive_user.clone(),
            tools: vec![],
        };
        let mut no_tools = |_: &mut crate::workflow::Context, _: &str, _: &str| ToolOutcome {
            content: "error: the archivist has no tools".into(),
            is_error: true,
        };
        let facts = match roles
            .archivist
            .generate(context, &archive, &mut no_tools, false)
        {
            Ok(text) => match parse_facts(&text) {
                Ok(f) => Ok(f),
                Err(e) => {
                    archive.user = format!(
                        "{archive_user}\n\nYour previous answer was invalid: {e}. Reply with only the JSON object."
                    );
                    roles
                        .archivist
                        .generate(context, &archive, &mut no_tools, false)
                        .and_then(|t| parse_facts(&t))
                }
            },
            Err(e) => Err(e),
        };
        match facts {
            Err(e) => context.emit(Event::Line(Kind::Note, format!("archivist: {e}"))),
            Ok(facts) => {
                for fact in facts {
                    let candidate = (|| -> Result<String, String> {
                        Ok(match &fact {
                            Fact::Entity { name, entity_type } => {
                                format!("new entity {name} ({entity_type})")
                            }
                            Fact::Aspect {
                                text, holder_id, ..
                            } => format!(
                                "aspect \"{text}\" on {}",
                                holder_id
                                    .map(|id| world.entity(id).map(|e| e.name))
                                    .transpose()?
                                    .unwrap_or_else(|| "the campaign".into())
                            ),
                            Fact::Relationship {
                                from_id,
                                to_id,
                                kind,
                                score,
                            } => format!(
                                "relationship {} → {} {kind} {score}",
                                world.entity(*from_id)?.name,
                                world.entity(*to_id)?.name
                            ),
                        })
                    })();
                    let candidate = match candidate {
                        Ok(c) => c,
                        Err(e) => {
                            context.emit(Event::Line(Kind::Note, format!("archivist: {e}")));
                            continue;
                        }
                    };
                    let answer = roles.compel_decider.decide(
                        context,
                        &DecisionQuery::Salient {
                            candidate: candidate.clone(),
                            scene_id,
                            present_names: ctx.present_names(),
                        },
                    );
                    match answer {
                        Ok(a) if a.yes => {
                            let result = match fact {
                                Fact::Entity { name, entity_type } => {
                                    world.add_entity(&name, &entity_type, None)
                                }
                                Fact::Aspect {
                                    text,
                                    scope,
                                    holder_id,
                                    slot,
                                } => world.add_aspect(AspectInput {
                                    text,
                                    scope,
                                    holder_id,
                                    slot,
                                    valid_from: None,
                                }),
                                Fact::Relationship {
                                    from_id,
                                    to_id,
                                    kind,
                                    score,
                                } => world.update_relationship(from_id, to_id, &kind, score, None),
                            };
                            if let Err(e) = result {
                                context.emit(Event::Line(
                                    Kind::Note,
                                    format!("archivist: {candidate}: {e}"),
                                ));
                            }
                        }
                        Ok(_) => {}
                        Err(e) => context.emit(Event::Line(
                            Kind::Note,
                            format!("archivist: {candidate}: {e}"),
                        )),
                    }
                }
            }
        }
        let changes = std::mem::take(&mut world.changes);
        drop(world);
        meta.turn += 1;
        store.stage_meta(&meta)?;
        txn.commit()?;
        (changes, narration, compels)
    };
    store.finish_meta(meta.clone())?;
    std::fs::write(store.dir().join(TRANSCRIPTS).join(format!("turn-{}.json",meta.turn)),serde_json::to_vec_pretty(&serde_json::json!({"turn":meta.turn,"action":action,"narration":narration,"compels":compels,"changes":changes})).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
    echo::write(store)?;
    if meta.turn % BACKUP_EVERY == 0 {
        store.checkpoint()?;
    }
    context.emit(Event::Line(
        Kind::Note,
        format!("turn {} · game_time {}", meta.turn, meta.game_time),
    ));
    for change in &changes {
        context.emit(Event::Line(Kind::Note, format!("  {change}")));
    }
    Ok(TurnReceipt {
        turn: meta.turn,
        game_time: meta.game_time,
        compels,
        changes,
        narration,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpg::test_support::*;
    use std::sync::Arc;
    fn cleanup(s: Session) {
        let root = s.context.project.root.clone();
        drop(s);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn a_scripted_turn_runs_end_to_end() {
        let mut roles = test_roles();
        roles.narrator = Box::new(ScriptedGen {
            steps: vec![vec![
                (
                    "add-aspect".into(),
                    r#"{"text":"Cornered","scope":"situation","holder_id":3}"#.into(),
                ),
                (
                    "update-relationship".into(),
                    r#"{"from":2,"to":1,"type":"enemy","score":-20}"#.into(),
                ),
            ]],
            finals: vec!["Mara watches.".into()],
            seen: Arc::default(),
        });
        roles.archivist =
            gen_text(r#"{"facts":[{"kind":"entity","name":"Mara","type":"character"}]}"#);
        let mut s = test_session("turn-end", roles);
        let r = run_turn(&mut s, "I wait").unwrap();
        assert_eq!(s.store.meta().turn, 1);
        assert!(r.changes.iter().any(|c| c.contains("Mara")));
        assert!(s.store.dir().join("transcripts/turn-1.json").exists());
        assert_eq!(
            s.store
                .db()
                .query_row("SELECT summary FROM events ORDER BY id LIMIT 1", [], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap(),
            "Player: I wait"
        );
        cleanup(s);
    }
    #[test]
    fn a_panic_in_archive_leaves_the_previous_turn() {
        let mut roles = test_roles();
        roles.archivist = gen_text("PANIC");
        let mut s = test_session("turn-panic", roles);
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_turn(&mut s, "Wait")))
                .is_err()
        );
        let dir = s
            .store
            .dir()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        drop(s.store);
        let store = crate::rpg::store::Store::open(&s.context.project, &dir).unwrap();
        assert_eq!(store.meta().turn, 0);
        assert_eq!(
            store
                .db()
                .query_row("SELECT count(*) FROM events", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(store);
        std::fs::remove_dir_all(s.context.project.root).unwrap();
    }
    #[test]
    fn a_narrator_error_rolls_back_the_turn() {
        let mut r = test_roles();
        r.narrator = gen_text("ERROR");
        let mut s = test_session("turn-error", r);
        assert!(run_turn(&mut s, "wait").is_err());
        assert_eq!(s.store.meta().turn, 0);
        assert_eq!(
            s.store
                .db()
                .query_row("SELECT count(*) FROM events", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        cleanup(s);
    }
    #[test]
    fn begin_scene_mid_turn_moves_the_next_turn() {
        let seen = Arc::default();
        let mut r = test_roles();
        r.narrator = Box::new(ScriptedGen {
            steps: vec![vec![(
                "begin-scene".into(),
                r#"{"title":"The Castle"}"#.into(),
            )]],
            finals: vec!["Enter".into(), "Wait".into()],
            seen: Arc::clone(&seen),
        });
        let mut s = test_session("turn-scene", r);
        run_turn(&mut s, "Enter").unwrap();
        run_turn(&mut s, "Wait").unwrap();
        assert!(seen.lock().unwrap()[1].system.contains("The Castle"));
        cleanup(s);
    }
    #[test]
    fn a_flashback_turn_sees_the_past() {
        let seen = Arc::default();
        let mut r = test_roles();
        r.narrator = Box::new(ScriptedGen {
            steps: vec![],
            finals: vec!["Past".into()],
            seen: Arc::clone(&seen),
        });
        let mut s = test_session("turn-past", r);
        let mut meta = s.store.meta().clone();
        {
            let mut w = World {
                db: s.store.db(),
                meta: &mut meta,
                changes: vec![],
            };
            w.log_event("1471-03-12T21:00", "later", None).unwrap();
            w.rename_aspect(1, "Cool-headed").unwrap();
            w.end_scene(3, &[]).unwrap();
            w.cut_to_scene(3).unwrap();
        }
        s.store.save_meta(meta).unwrap();
        run_turn(&mut s, "Remember").unwrap();
        let seen = seen.lock().unwrap();
        assert!(seen[0].system.contains("FLASHBACK"));
        assert!(seen[0].system.contains("Hot-headed"));
        assert!(!seen[0].system.contains("Cool-headed"));
        drop(seen);
        cleanup(s);
    }
    #[test]
    fn compel_flags_reach_the_narrator() {
        let seen = Arc::default();
        let mut r = test_roles();
        r.narrator = Box::new(ScriptedGen {
            steps: vec![],
            finals: vec!["Blade".into()],
            seen: Arc::clone(&seen),
        });
        let mut s = test_session("turn-compel", r);
        run_turn(&mut s, "I draw my blade").unwrap();
        assert!(
            seen.lock().unwrap()[0]
                .system
                .contains("## Suggested compels")
        );
        cleanup(s);
    }
    #[test]
    fn invalid_archivist_json_is_retried_then_dropped() {
        let mut r = test_roles();
        r.archivist = Box::new(ScriptedGen {
            steps: vec![],
            finals: vec!["nonsense".into(), "nonsense".into()],
            seen: Arc::default(),
        });
        let mut s = test_session("turn-invalid", r);
        let feed = Arc::new(crate::web_util::Feed::default());
        *s.current.lock().expect("current lock") = Some(feed.clone());
        run_turn(&mut s, "wait").unwrap();
        assert_eq!(s.store.meta().turn, 1);
        assert!(
            feed.snapshot()
                .events
                .iter()
                .any(|e| matches!(e,Event::Line(Kind::Note,t) if t.contains("archivist:")))
        );
        cleanup(s);
    }
    #[test]
    fn parse_facts_strips_fences() {
        assert_eq!(parse_facts("```json\n{\"facts\":[]}\n```").unwrap(), vec![]);
        assert!(parse_facts("nonsense").is_err());
    }
    #[test]
    fn echo_matches_the_db_after_turns() {
        let mut s = test_session("turn-echo", test_roles());
        run_turn(&mut s, "wait").unwrap();
        run_turn(&mut s, "wait").unwrap();
        assert_eq!(
            std::fs::read_to_string(s.store.dir().join("events.jsonl")).unwrap(),
            crate::rpg::echo::render(s.store.db()).unwrap()
        );
        cleanup(s);
    }
    #[test]
    fn every_tenth_turn_creates_a_backup() {
        let mut s = test_session("turn-backup", test_roles());
        for _ in 0..9 {
            run_turn(&mut s, "I wait").unwrap();
        }
        assert!(!s.store.dir().join("world.vacuum").exists());
        run_turn(&mut s, "I wait").unwrap();
        assert!(s.store.dir().join("world.vacuum").exists());
        cleanup(s);
    }
}

//! Scenes, temporal presence, and bounded narrator context.
use super::{
    schema::*,
    store::Meta,
    world::{World, aspect_text, atomic, rows, text_limit},
};
use rusqlite::{Connection, OptionalExtension, params};
/// An entity and its role in a scene.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq)]
pub struct Participant {
    pub entity_id: i64,
    pub role: String,
}
/// Arguments for beginning a scene.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SceneInput {
    pub title: String,
    pub summary: Option<String>,
    pub location_id: Option<i64>,
    pub chapter: Option<i64>,
    #[serde(default)]
    pub participants: Vec<Participant>,
    #[serde(default)]
    pub situation_aspects: Vec<String>,
}
/// One present entity with its applicable statistics.
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub struct PresentEntity {
    pub entity: EntityRow,
    pub role: String,
    pub aspects: Vec<AspectRow>,
    pub skills: Vec<SkillRow>,
    pub stress: Vec<StressTrackRow>,
    pub behaviors: Vec<BehaviorRow>,
    pub relationships: Vec<RelationshipRow>,
    pub fate_points: Option<i64>,
}
/// A bounded, possibly historical bundle of world facts for narration.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Default)]
pub struct SceneContext {
    pub scene: Option<SceneRow>,
    pub location: Option<EntityRow>,
    pub flashback: bool,
    pub as_of: String,
    pub campaign_aspects: Vec<AspectRow>,
    pub situation_aspects: Vec<AspectRow>,
    pub location_aspects: Vec<AspectRow>,
    pub present: Vec<PresentEntity>,
    pub recent_events: Vec<EventRow>,
    pub recent_narration: Vec<String>,
}
fn role(role: &str) -> Result<(), String> {
    if ["pc", "npc", "prop", "observer"].contains(&role) {
        Ok(())
    } else {
        Err("role must be pc, npc, prop or observer".into())
    }
}
impl World<'_> {
    /// Loads a scene using its entity id.
    pub fn scene(&self, id: i64) -> Result<SceneRow, String> {
        self.db
            .query_row(
                &format!("SELECT {} FROM scenes WHERE id=?1", SceneRow::COLUMNS),
                [id],
                SceneRow::from_row,
            )
            .optional()
            .map_err(sql_error)?
            .ok_or_else(|| format!("no scene #{id}"))
    }
    /// Begins a scene with validated participants and situation aspects.
    pub fn begin_scene(&mut self, input: SceneInput) -> Result<i64, String> {
        atomic(self.db, || {
            let title = text_limit(&input.title, "scene title", 80)?;
            if let Some(id) = input.location_id {
                self.entity(id)?;
            }
            let chapter = match input.chapter {
                Some(n) => n,
                None => match self.meta.current_scene_id {
                    Some(id) => self.scene(id)?.chapter,
                    None => 1,
                },
            };
            if chapter < 1 {
                return Err("chapter must be at least 1".into());
            }
            let mut ids = std::collections::HashSet::new();
            for p in &input.participants {
                self.entity(p.entity_id)?;
                role(&p.role)?;
                if !ids.insert(p.entity_id) {
                    return Err("a participant appears more than once".into());
                }
            }
            for text in &input.situation_aspects {
                aspect_text(text)?;
            }
            let event = self.latest_event_id()?;
            self.db
                .execute(
                    "INSERT INTO entities(name,type,created_event_id) VALUES(?1,'scene',?2)",
                    params![title, event],
                )
                .map_err(sql_error)?;
            let id = self.db.last_insert_rowid();
            self.db.execute("INSERT INTO scenes(id,title,location_entity_id,chapter,summary,started_event_id,valid_from,chain_root_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?1)",params![id,title,input.location_id,chapter,input.summary,event,self.now()]).map_err(sql_error)?;
            for p in &input.participants {
                self.insert_presence(p.entity_id, id, &p.role)?;
            }
            // Insert directly after validating everything so nested SQL cannot leave meta or change receipts behind.
            for text in &input.situation_aspects {
                self.insert_aspect(
                    aspect_text(text)?,
                    "situation",
                    Some(id),
                    None,
                    self.now(),
                    None,
                )?;
            }
            self.meta.current_scene_id = Some(id);
            self.changes.push(format!("scene #{id}  \"{title}\" begun"));
            Ok(id)
        })
    }
    fn insert_presence(&self, entity: i64, scene: i64, role: &str) -> Result<i64, String> {
        self.db.execute("INSERT INTO presence(entity_id,scene_id,role,valid_from,joined_event_id) VALUES(?1,?2,?3,?4,?5)",params![entity,scene,role,self.now(),self.latest_event_id()?]).map_err(sql_error)?;
        let id = self.db.last_insert_rowid();
        self.db
            .execute("UPDATE presence SET chain_root_id=id WHERE id=?1", [id])
            .map_err(sql_error)?;
        Ok(id)
    }
    /// Joins an open scene, rejecting duplicate presence.
    pub fn join_scene(
        &mut self,
        entity: i64,
        scene: i64,
        as_role: Option<&str>,
    ) -> Result<i64, String> {
        atomic(self.db, || {
            let e = self.entity(entity)?;
            if self.scene(scene)?.valid_to.is_some() {
                return Err(format!("scene #{scene} has ended"));
            }
            let r = as_role.unwrap_or("npc");
            role(r)?;
            let present:bool=self.db.query_row("SELECT EXISTS(SELECT 1 FROM presence WHERE entity_id=?1 AND scene_id=?2 AND valid_to IS NULL)",params![entity,scene],|r|r.get(0)).map_err(sql_error)?;
            if present {
                return Err(format!("{} is already in the scene", e.name));
            }
            let id = self.insert_presence(entity, scene, r)?;
            self.changes
                .push(format!("{} joined scene #{scene}", e.name));
            Ok(id)
        })
    }
    /// Closes only this entity's presence in the specified scene.
    pub fn leave_scene(&mut self, entity: i64, scene: i64) -> Result<(), String> {
        atomic(self.db, || {
            let e = self.entity(entity)?;
            self.scene(scene)?;
            let n=self.db.execute("UPDATE presence SET valid_to=?1,left_event_id=?2 WHERE entity_id=?3 AND scene_id=?4 AND valid_to IS NULL",params![self.now(),self.latest_event_id()?,entity,scene]).map_err(sql_error)?;
            if n == 0 {
                return Err(format!("{} is not in the scene", e.name));
            }
            self.changes.push(format!("{} left scene #{scene}", e.name));
            Ok(())
        })
    }
    /// Ends a scene, moving selected aspects to its location and expiring the rest.
    pub fn end_scene(&mut self, id: i64, carry_over: &[i64]) -> Result<(), String> {
        atomic(self.db, || {
            let scene = self.scene(id)?;
            if scene.valid_to.is_some() {
                return Err(format!("scene #{id} has ended"));
            }
            let aspects = self
                .aspects(Some(id))?
                .into_iter()
                .filter(|a| a.scope == "situation")
                .collect::<Vec<_>>();
            for aspect in carry_over {
                if !aspects.iter().any(|a| a.id == *aspect) {
                    return Err(format!(
                        "aspect #{aspect} is not a current situation aspect of scene #{id}"
                    ));
                }
            }
            if !carry_over.is_empty() && scene.location_entity_id.is_none() {
                return Err(format!("scene #{id} has no location to carry aspects to"));
            }
            if aspects.iter().any(|a| a.valid_from.as_str() > self.now()) {
                return Err("cannot end a scene before its aspects begin".into());
            }
            let mut carried = 0;
            let mut expired = 0;
            let event = self.latest_event_id()?;
            for a in aspects {
                if carry_over.contains(&a.id) {
                    self.insert_aspect(
                        &a.text,
                        &a.scope,
                        scene.location_entity_id,
                        a.slot.as_deref(),
                        self.now(),
                        Some(&a),
                    )?;
                    carried += 1;
                } else {
                    self.db
                        .execute(
                            "UPDATE aspects SET valid_to=?1,ended_event_id=?2 WHERE id=?3",
                            params![self.now(), event, a.id],
                        )
                        .map_err(sql_error)?;
                    expired += 1;
                }
            }
            self.db.execute("UPDATE presence SET valid_to=?1,left_event_id=?2 WHERE scene_id=?3 AND valid_to IS NULL",params![self.now(),event,id]).map_err(sql_error)?;
            self.db
                .execute(
                    "UPDATE scenes SET valid_to=?1,ended_event_id=?2 WHERE id=?3",
                    params![self.now(), event, id],
                )
                .map_err(sql_error)?;
            if self.meta.current_scene_id == Some(id) {
                self.meta.current_scene_id = None;
            }
            self.changes.push(format!(
                "scene #{id}  ended ({carried} carried, {expired} expired)"
            ));
            Ok(())
        })
    }
    /// Selects an existing scene, including a closed scene for flashbacks.
    pub fn cut_to_scene(&mut self, id: i64) -> Result<(), String> {
        atomic(self.db, || {
            self.scene(id)?;
            self.meta.current_scene_id = Some(id);
            self.changes.push(format!("cut to scene #{id}"));
            Ok(())
        })
    }
}
/// Loads the world at a scene's start for flashbacks, or current rows for an open scene.
pub fn get_scene_context(
    db: &Connection,
    meta: &Meta,
    scene_id: Option<i64>,
    narrations: Vec<String>,
) -> Result<SceneContext, String> {
    let mut m = meta.clone();
    let w = World {
        db,
        meta: &mut m,
        changes: vec![],
    };
    let scene = scene_id.map(|id| w.scene(id)).transpose()?;
    let flashback = scene.as_ref().is_some_and(|s| s.valid_to.is_some());
    let as_of = if flashback {
        scene
            .as_ref()
            .map(|s| s.valid_from.clone())
            .unwrap_or_default()
    } else {
        meta.game_time.clone()
    };
    let aspect_query = |holder| {
        if flashback {
            w.aspects_at(holder, &as_of)
        } else {
            w.aspects(holder)
        }
    };
    let mut ctx = SceneContext {
        as_of: as_of.clone(),
        flashback,
        campaign_aspects: aspect_query(None)?,
        recent_narration: narrations,
        ..Default::default()
    };
    let Some(scene) = scene else { return Ok(ctx) };
    ctx.location = scene
        .location_entity_id
        .map(|id| w.entity(id))
        .transpose()?;
    if let Some(l) = &ctx.location {
        ctx.location_aspects = aspect_query(Some(l.id))?;
    }
    ctx.situation_aspects = aspect_query(Some(scene.id))?
        .into_iter()
        .filter(|a| a.scope == "situation")
        .collect();
    let window = if flashback {
        "valid_from<=?2 AND (valid_to IS NULL OR valid_to>?2)"
    } else {
        "valid_to IS NULL AND superseded_by IS NULL AND ?2 IS NOT NULL"
    };
    let presence = rows(
        db,
        &format!(
            "SELECT {} FROM presence WHERE scene_id=?1 AND {window} ORDER BY id",
            PresenceRow::COLUMNS
        ),
        params![scene.id, as_of],
        PresenceRow::from_row,
    )?;
    let ids = presence.iter().map(|p| p.entity_id).collect::<Vec<_>>();
    for p in presence {
        let entity = w.entity(p.entity_id)?;
        let fate_points = if entity.kind == "character" {
            Some(if flashback {
                db.query_row("SELECT coalesce(sum(delta),0) FROM fate_transactions WHERE entity_id=?1 AND game_time<=?2",params![entity.id,as_of],|r|r.get(0)).map_err(sql_error)?
            } else {
                w.fate_balance(entity.id)?
            })
        } else {
            None
        };
        let skills = rows(
            db,
            &format!(
                "SELECT {} FROM skills WHERE entity_id=?1 AND {window} ORDER BY id",
                SkillRow::COLUMNS
            ),
            params![entity.id, as_of],
            SkillRow::from_row,
        )?;
        let stress = rows(
            db,
            &format!(
                "SELECT {} FROM stress_tracks WHERE entity_id=?1 AND {window} ORDER BY id",
                StressTrackRow::COLUMNS
            ),
            params![entity.id, as_of],
            StressTrackRow::from_row,
        )?;
        let behaviors = rows(
            db,
            &format!(
                "SELECT {} FROM behaviors WHERE entity_id=?1 AND {window} ORDER BY id",
                BehaviorRow::COLUMNS
            ),
            params![entity.id, as_of],
            BehaviorRow::from_row,
        )?;
        let relationships = rows(
            db,
            &format!(
                "SELECT {} FROM relationships WHERE from_entity_id=?1 AND {window} ORDER BY id",
                RelationshipRow::COLUMNS
            ),
            params![entity.id, as_of],
            RelationshipRow::from_row,
        )?
        .into_iter()
        .filter(|r| ids.contains(&r.to_entity_id))
        .collect();
        ctx.present.push(PresentEntity {
            aspects: aspect_query(Some(entity.id))?,
            entity,
            role: p.role,
            skills,
            stress,
            behaviors,
            relationships,
            fate_points,
        });
    }
    ctx.recent_events = rows(
        db,
        &format!(
            "SELECT {} FROM events WHERE id>coalesce(?1,0) AND (?2 IS NULL OR id<=?2) ORDER BY id DESC LIMIT 10",
            EventRow::COLUMNS
        ),
        params![
            scene.started_event_id,
            if flashback {
                scene.ended_event_id
            } else {
                None
            }
        ],
        EventRow::from_row,
    )?;
    ctx.recent_events.reverse();
    ctx.scene = Some(scene);
    Ok(ctx)
}
fn aspect_list(aspects: &[AspectRow], slots: bool) -> String {
    aspects
        .iter()
        .map(|a| {
            format!(
                "#{} {}{}",
                a.id,
                a.text,
                if slots {
                    a.slot
                        .as_ref()
                        .map(|s| format!(" [{s}]"))
                        .unwrap_or_default()
                } else {
                    String::new()
                }
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}
impl SceneContext {
    /// Formats the bounded facts as compact narrator prompt text.
    pub fn render(&self) -> String {
        let mut lines = vec![match &self.scene {
            Some(s) => format!(
                "Scene #{}: {} (chapter {}){}",
                s.id,
                s.title,
                s.chapter,
                if self.flashback {
                    format!(" [FLASHBACK — the world as of {}]", self.as_of)
                } else {
                    String::new()
                }
            ),
            None => "No scene is active. Call begin-scene to start one.".into(),
        }];
        if let Some(s) = &self.scene {
            lines.push(format!(
                "Summary: {}",
                s.summary.as_deref().unwrap_or("(none)")
            ));
        }
        if let Some(l) = &self.location {
            lines.push(format!(
                "Location: {}{}",
                l.name,
                if self.location_aspects.is_empty() {
                    String::new()
                } else {
                    format!(
                        " — {}",
                        self.location_aspects
                            .iter()
                            .map(|a| a.text.as_str())
                            .collect::<Vec<_>>()
                            .join("; ")
                    )
                }
            ));
        }
        if self.scene.is_some() {
            lines.push(format!(
                "Situation: {}",
                aspect_list(&self.situation_aspects, false)
            ));
        }
        if !self.campaign_aspects.is_empty() {
            lines.push(format!(
                "Campaign: {}",
                aspect_list(&self.campaign_aspects, false)
            ));
        }
        if self.scene.is_some() {
            lines.push("Present:".into());
        }
        for p in &self.present {
            let mut line = format!(
                "- #{} {} ({}, {})",
                p.entity.id, p.entity.name, p.role, p.entity.kind
            );
            if !p.aspects.is_empty() {
                line += &format!(" — {}", aspect_list(&p.aspects, true));
            }
            if !p.skills.is_empty() {
                line += &format!(
                    " | skills {}",
                    p.skills
                        .iter()
                        .map(|s| format!("{} +{}", s.name, s.rank))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if !p.stress.is_empty() {
                line += &format!(
                    " | stress {}",
                    p.stress
                        .iter()
                        .map(|s| format!("{} {}/{}", s.name, s.marked, s.size))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if let Some(f) = p.fate_points {
                line += &format!(" | fate {f}");
            }
            if !p.behaviors.is_empty() {
                line += &format!(
                    " | {}",
                    p.behaviors
                        .iter()
                        .map(|b| format!("when {}: {}", b.trigger, b.action))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if !p.relationships.is_empty() {
                line += &format!(
                    " | {}",
                    p.relationships
                        .iter()
                        .map(|r| format!(
                            "→ {} {} {}",
                            self.present
                                .iter()
                                .find(|p| p.entity.id == r.to_entity_id)
                                .map(|p| p.entity.name.as_str())
                                .unwrap_or("?"),
                            r.kind,
                            r.score
                        ))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            lines.push(line);
        }
        if !self.recent_events.is_empty() {
            lines.push("Recent events:".into());
            lines.extend(
                self.recent_events
                    .iter()
                    .map(|e| format!("- #{} {} {}", e.id, e.game_time, e.summary)),
            );
        }
        if !self.recent_narration.is_empty() {
            lines.push("Recent narration:".into());
            lines.push(self.recent_narration.join("\n\n"));
        }
        lines.join("\n")
    }
    /// Joins scene prose for decision backends.
    pub fn scene_text(&self) -> String {
        let mut parts = vec![];
        if let Some(s) = &self.scene {
            parts.push(s.title.clone());
            if let Some(summary) = &s.summary {
                parts.push(summary.clone());
            }
        }
        parts.extend(self.situation_aspects.iter().map(|a| a.text.clone()));
        parts.join(". ")
    }
    /// Lists situation and present-entity aspects in stable order.
    pub fn compel_candidates(&self) -> Vec<&AspectRow> {
        self.situation_aspects
            .iter()
            .chain(self.present.iter().flat_map(|p| p.aspects.iter()))
            .collect()
    }
    /// Lists present entity names in presence order.
    pub fn present_names(&self) -> Vec<String> {
        self.present.iter().map(|p| p.entity.name.clone()).collect()
    }
}
#[cfg(test)]
mod tests {
    use super::super::world::{AspectInput, Scope};
    use super::*;
    fn db() -> (rusqlite::Connection, Meta) {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        super::super::schema::migrate(&db).unwrap();
        (
            db,
            Meta {
                name: "Test".into(),
                created: "now".into(),
                turn: 0,
                game_time: super::super::store::START_TIME.into(),
                current_scene_id: None,
                roles: Default::default(),
            },
        )
    }
    fn input(location_id: Option<i64>, pc: i64) -> SceneInput {
        SceneInput {
            title: "Bridge".into(),
            summary: Some("Midnight crossing".into()),
            location_id,
            chapter: None,
            participants: vec![Participant {
                entity_id: pc,
                role: "pc".into(),
            }],
            situation_aspects: vec!["Rain".into(), "Wind".into()],
        }
    }
    #[test]
    fn begin_scene_creates_the_scene_presence_and_aspects() {
        let (db, mut m) = db();
        let mut w = World {
            db: &db,
            meta: &mut m,
            changes: vec![],
        };
        let a = w.add_entity("A", "character", None).unwrap();
        let b = w.add_entity("B", "character", None).unwrap();
        let l = w.add_entity("Bridge", "location", None).unwrap();
        let mut i = input(Some(l), a);
        i.participants.push(Participant {
            entity_id: b,
            role: "npc".into(),
        });
        let s = w.begin_scene(i).unwrap();
        assert_eq!(w.entity(s).unwrap().kind, "scene");
        assert_eq!(w.scene(s).unwrap().id, s);
        assert_eq!(w.aspects(Some(s)).unwrap().len(), 2);
        assert_eq!(w.meta.current_scene_id, Some(s));
        let c = get_scene_context(&db, w.meta, Some(s), vec![]).unwrap();
        assert_eq!(c.present.len(), 2);
    }
    #[test]
    fn end_scene_carries_listed_aspects_and_expires_the_rest() {
        let (db, mut m) = db();
        let mut w = World {
            db: &db,
            meta: &mut m,
            changes: vec![],
        };
        let a = w.add_entity("A", "character", None).unwrap();
        let l = w.add_entity("Bridge", "location", None).unwrap();
        let s = w.begin_scene(input(Some(l), a)).unwrap();
        let aspects = w.aspects(Some(s)).unwrap();
        let other = w.begin_scene(input(None, a)).unwrap();
        w.cut_to_scene(s).unwrap();
        w.end_scene(s, &[aspects[0].id]).unwrap();
        assert_eq!(w.meta.current_scene_id, None);
        let carry = w.aspects(Some(l)).unwrap();
        assert_eq!(carry.len(), 1);
        assert_eq!(carry[0].chain_root_id, aspects[0].chain_root_id);
        assert!(w.aspects(Some(s)).unwrap().is_empty());
        assert!(
            w.aspect_history(aspects[1].id).unwrap()[0]
                .superseded_by
                .is_none()
        );
        assert_eq!(
            get_scene_context(&db, w.meta, Some(other), vec![])
                .unwrap()
                .present
                .len(),
            1
        );
    }
    #[test]
    fn end_scene_without_location_rejects_carry_over() {
        let (db, mut m) = db();
        let mut w = World {
            db: &db,
            meta: &mut m,
            changes: vec![],
        };
        let a = w.add_entity("A", "character", None).unwrap();
        let s = w.begin_scene(input(None, a)).unwrap();
        let a = w.aspects(Some(s)).unwrap()[0].id;
        assert!(w.end_scene(s, &[a]).is_err());
        assert!(w.scene(s).unwrap().valid_to.is_none());
        assert_eq!(w.aspects(Some(s)).unwrap().len(), 2);
    }
    #[test]
    fn join_and_leave_are_temporal() {
        let (db, mut m) = db();
        let mut w = World {
            db: &db,
            meta: &mut m,
            changes: vec![],
        };
        let a = w.add_entity("A", "character", None).unwrap();
        let b = w.add_entity("B", "character", None).unwrap();
        let s = w.begin_scene(input(None, a)).unwrap();
        let p = w.join_scene(b, s, None).unwrap();
        assert!(w.join_scene(b, s, None).is_err());
        w.log_event("1471-03-12T21:00", "time", None).unwrap();
        w.leave_scene(b, s).unwrap();
        assert_eq!(
            db.query_row("SELECT valid_to FROM presence WHERE id=?1", [p], |r| r
                .get::<_, String>(
                0
            ))
            .unwrap(),
            "1471-03-12T21:00"
        );
    }
    #[test]
    fn a_closed_scene_loads_as_of_its_start() {
        let (db, mut m) = db();
        let mut w = World {
            db: &db,
            meta: &mut m,
            changes: vec![],
        };
        let a = w.add_entity("A", "character", None).unwrap();
        let old = w
            .add_aspect(AspectInput {
                text: "Old".into(),
                scope: Scope::Character,
                holder_id: Some(a),
                slot: None,
                valid_from: None,
            })
            .unwrap();
        let s = w.begin_scene(input(None, a)).unwrap();
        w.log_event("1471-03-12T21:00", "time", None).unwrap();
        w.rename_aspect(old, "New").unwrap();
        w.add_aspect(AspectInput {
            text: "Later".into(),
            scope: Scope::Situation,
            holder_id: Some(s),
            slot: None,
            valid_from: None,
        })
        .unwrap();
        w.log_event("1471-03-12T22:00", "time", None).unwrap();
        w.end_scene(s, &[]).unwrap();
        w.begin_scene(input(None, a)).unwrap();
        let c = get_scene_context(&db, w.meta, Some(s), vec![]).unwrap();
        assert!(c.flashback);
        assert_eq!(c.present[0].aspects[0].text, "Old");
        assert_eq!(c.situation_aspects.len(), 2);
    }
    #[test]
    fn recent_events_belong_to_the_scene() {
        let (db, mut m) = db();
        let mut w = World {
            db: &db,
            meta: &mut m,
            changes: vec![],
        };
        let a = w.add_entity("A", "character", None).unwrap();
        w.log_event("1471-03-12T20:00", "before", None).unwrap();
        let s = w.begin_scene(input(None, a)).unwrap();
        for n in 0..12 {
            w.log_event("1471-03-12T20:00", &format!("event {n}"), None)
                .unwrap();
        }
        let c = get_scene_context(&db, w.meta, Some(s), vec![]).unwrap();
        assert_eq!(c.recent_events.len(), 10);
        assert_eq!(c.recent_events[0].summary, "event 2");
        assert_eq!(c.recent_events[9].summary, "event 11");
    }
    #[test]
    fn render_without_a_scene_says_so() {
        assert_eq!(
            SceneContext::default().render(),
            "No scene is active. Call begin-scene to start one."
        );
    }
    #[test]
    fn render_matches_the_snapshot() {
        let (db, mut m) = db();
        let mut w = World {
            db: &db,
            meta: &mut m,
            changes: vec![],
        };
        let a = w.add_entity("Aldric", "character", None).unwrap();
        let b = w.add_entity("Gareth", "character", None).unwrap();
        let l = w.add_entity("Ashford Bridge", "location", None).unwrap();
        for (text, holder, scope, slot) in [
            ("Knight", Some(a), Scope::Character, Some("high-concept")),
            ("Hot-headed", Some(a), Scope::Character, Some("trouble")),
            ("Ancient", Some(l), Scope::Situation, None),
            ("Empire Falling", None, Scope::Campaign, None),
        ] {
            w.add_aspect(AspectInput {
                text: text.into(),
                holder_id: holder,
                scope,
                slot: slot.map(str::to_owned),
                valid_from: None,
            })
            .unwrap();
        }
        w.set_skill(a, "Fight", 3, None, None).unwrap();
        w.add_stress_track(a, "Physical", 3).unwrap();
        w.award_fate(a, 3, "refresh", None).unwrap();
        w.update_relationship(a, b, "enemy", -10, None).unwrap();
        let mut i = input(Some(l), a);
        i.participants.push(Participant {
            entity_id: b,
            role: "npc".into(),
        });
        let s = w.begin_scene(i).unwrap();
        w.log_event("1471-03-12T20:05", "Player: I draw my blade", None)
            .unwrap();
        let c = get_scene_context(&db, w.meta, Some(s), vec!["The bridge groans.".into()]).unwrap();
        assert_eq!(
            c.render(),
            "Scene #4: Bridge (chapter 1)\nSummary: Midnight crossing\nLocation: Ashford Bridge — Ancient\nSituation: #5 Rain; #6 Wind\nCampaign: #4 Empire Falling\nPresent:\n- #1 Aldric (pc, character) — #1 Knight [high-concept]; #2 Hot-headed [trouble] | skills Fight +3 | stress Physical 0/3 | fate 3 | → Gareth enemy -10\n- #2 Gareth (npc, character) | fate 0\nRecent events:\n- #1 1471-03-12T20:05 Player: I draw my blade\nRecent narration:\nThe bridge groans."
        );
    }
}

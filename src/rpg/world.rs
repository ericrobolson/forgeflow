//! Validated temporal operations on an RPG world.
use super::{schema::*, store::Meta};
use rusqlite::{Connection, OptionalExtension, params};
/// A world connection and the metadata being edited during this turn.
pub struct World<'a> {
    pub db: &'a Connection,
    pub meta: &'a mut Meta,
    pub changes: Vec<String>,
}
struct Savepoint<'a> {
    db: &'a Connection,
    done: bool,
}
impl Drop for Savepoint<'_> {
    fn drop(&mut self) {
        if !self.done {
            let _ = self
                .db
                .execute_batch("ROLLBACK TO world_op; RELEASE world_op");
        }
    }
}
/// Runs an operation in a savepoint, rolling it back on errors or panics.
pub fn atomic<T>(db: &Connection, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    db.execute_batch("SAVEPOINT world_op").map_err(sql_error)?;
    let mut guard = Savepoint { db, done: false };
    let result = f()?;
    db.execute_batch("RELEASE world_op").map_err(sql_error)?;
    guard.done = true;
    Ok(result)
}
/// Checks the fixed-width in-world calendar format YYYY-MM-DDTHH:MM.
pub fn check_game_time(text: &str) -> Result<(), String> {
    let b = text.as_bytes();
    if b.len() != 16
        || b.iter().enumerate().any(|(i, c)| match i {
            4 | 7 => *c != b'-',
            10 => *c != b'T',
            13 => *c != b':',
            _ => !c.is_ascii_digit(),
        })
    {
        return Err("game time must be YYYY-MM-DDTHH:MM".into());
    }
    let number = |i: usize| u16::from(b[i] - b'0') * 10 + u16::from(b[i + 1] - b'0');
    if !(1..=12).contains(&number(5))
        || !(1..=31).contains(&number(8))
        || number(11) > 23
        || number(14) > 59
    {
        return Err("game time has an invalid month, day, hour or minute".into());
    }
    Ok(())
}
/// The Fate aspect categories supported by the world.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Character,
    Situation,
    Campaign,
    Item,
    Consequence,
}
impl Scope {
    /// The stored spelling of this scope.
    pub fn name(self) -> &'static str {
        match self {
            Self::Character => "character",
            Self::Situation => "situation",
            Self::Campaign => "campaign",
            Self::Item => "item",
            Self::Consequence => "consequence",
        }
    }
    /// Parses a supported aspect scope.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "character" => Ok(Self::Character),
            "situation" => Ok(Self::Situation),
            "campaign" => Ok(Self::Campaign),
            "item" => Ok(Self::Item),
            "consequence" => Ok(Self::Consequence),
            _ => Err(format!("unknown aspect scope {s}")),
        }
    }
}
/// Arguments for adding a temporal aspect.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct AspectInput {
    pub text: String,
    pub scope: Scope,
    pub holder_id: Option<i64>,
    pub slot: Option<String>,
    pub valid_from: Option<String>,
}
pub(crate) fn text_limit<'a>(s: &'a str, name: &str, max: usize) -> Result<&'a str, String> {
    let s = s.trim();
    if s.is_empty() || s.chars().count() > max {
        Err(format!("{name} must be 1 to {max} characters"))
    } else {
        Ok(s)
    }
}
pub(crate) fn aspect_text(s: &str) -> Result<&str, String> {
    let s = s.trim();
    if s.is_empty() {
        Err("aspect text is empty".into())
    } else if s.chars().count() > 80 {
        Err("aspect text is over 80 characters; an aspect is a short phrase".into())
    } else {
        Ok(s)
    }
}
pub(crate) fn rows<T>(
    db: &Connection,
    sql: &str,
    args: impl rusqlite::Params,
    read: fn(&rusqlite::Row) -> rusqlite::Result<T>,
) -> Result<Vec<T>, String> {
    db.prepare(sql)
        .map_err(sql_error)?
        .query_map(args, read)
        .map_err(sql_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_error)
}
impl World<'_> {
    /// The current game clock.
    pub fn now(&self) -> &str {
        &self.meta.game_time
    }
    /// The number of the turn in progress.
    pub fn turn(&self) -> i64 {
        self.meta.turn + 1
    }
    /// The most recently inserted event, if any.
    pub fn latest_event_id(&self) -> Result<Option<i64>, String> {
        self.db
            .query_row("SELECT max(id) FROM events", [], |r| r.get(0))
            .map_err(sql_error)
    }
    pub(crate) fn valid_from(&self, time: Option<&str>) -> Result<String, String> {
        let v = time.unwrap_or(self.now());
        check_game_time(v)?;
        if v < self.now() {
            return Err(format!(
                "valid_from {v} is before the current game time {}",
                self.now()
            ));
        }
        Ok(v.into())
    }
    /// Adds an event, advancing the clock only when its time is later.
    pub fn log_event(
        &mut self,
        time: &str,
        summary: &str,
        actor: Option<i64>,
    ) -> Result<i64, String> {
        atomic(self.db, || {
            check_game_time(time)?;
            let summary = text_limit(summary, "event summary", 300)?;
            if let Some(id) = actor {
                self.entity(id)?;
            }
            self.db.execute("INSERT INTO events(turn,game_time,summary,actor_entity_id) VALUES(?1,?2,?3,?4)",params![self.turn(),time,summary,actor]).map_err(sql_error)?;
            let id = self.db.last_insert_rowid();
            if time > self.now() {
                self.meta.game_time = time.into();
            }
            self.changes.push(format!("event #{id}  {summary}"));
            Ok(id)
        })
    }
    /// Adds any kind of entity, including characters, locations and hazards.
    pub fn add_entity(
        &mut self,
        name: &str,
        kind: &str,
        attributes: Option<serde_json::Value>,
    ) -> Result<i64, String> {
        atomic(self.db, || {
            let name = text_limit(name, "entity name", 80)?;
            let kind = kind.trim().to_lowercase();
            text_limit(&kind, "entity type", 40)?;
            if !kind
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-')
            {
                return Err("entity type allows only a-z, 0-9, _ and -".into());
            }
            if attributes.as_ref().is_some_and(|a| !a.is_object()) {
                return Err("entity attributes must be a JSON object".into());
            }
            self.db.execute("INSERT INTO entities(name,type,attributes,created_event_id) VALUES(?1,?2,?3,?4)",params![name,kind,attributes.map(|a|a.to_string()),self.latest_event_id()?]).map_err(sql_error)?;
            let id = self.db.last_insert_rowid();
            self.changes.push(format!("entity #{id}  {name} ({kind})"));
            Ok(id)
        })
    }
    /// Loads one entity by id.
    pub fn entity(&self, id: i64) -> Result<EntityRow, String> {
        self.db
            .query_row(
                &format!("SELECT {} FROM entities WHERE id=?1", EntityRow::COLUMNS),
                [id],
                EntityRow::from_row,
            )
            .optional()
            .map_err(sql_error)?
            .ok_or_else(|| format!("no entity #{id}"))
    }
    /// Finds the first case-insensitive exact entity name.
    pub fn find_entity(&self, name: &str) -> Result<Option<EntityRow>, String> {
        self.db
            .query_row(
                &format!(
                    "SELECT {} FROM entities WHERE name=?1 COLLATE NOCASE ORDER BY id LIMIT 1",
                    EntityRow::COLUMNS
                ),
                [name],
                EntityRow::from_row,
            )
            .optional()
            .map_err(sql_error)
    }
    pub(crate) fn aspect(&self, id: i64) -> Result<AspectRow, String> {
        self.db
            .query_row(
                &format!("SELECT {} FROM aspects WHERE id=?1", AspectRow::COLUMNS),
                [id],
                AspectRow::from_row,
            )
            .optional()
            .map_err(sql_error)?
            .ok_or_else(|| format!("no aspect #{id}"))
    }
    pub(crate) fn current_aspect(&self, id: i64) -> Result<AspectRow, String> {
        let a = self
            .aspect(id)
            .map_err(|_| format!("aspect #{id} is not current"))?;
        if a.valid_to.is_some() || a.superseded_by.is_some() {
            return Err(format!("aspect #{id} is not current"));
        }
        Ok(a)
    }
    /// Adds an aspect, replacing only a matching slotted predecessor.
    pub fn add_aspect(&mut self, input: AspectInput) -> Result<i64, String> {
        atomic(self.db, || {
            let text = aspect_text(&input.text)?;
            let scope = input.scope.name();
            if input.scope == Scope::Campaign && input.holder_id.is_some() {
                return Err("a campaign aspect has no holder".into());
            }
            if input.scope != Scope::Campaign && input.holder_id.is_none() {
                return Err(format!("a {scope} aspect needs a holder_id"));
            }
            if let Some(id) = input.holder_id {
                self.entity(id)?;
            }
            if input.scope == Scope::Consequence && input.slot.is_none() {
                return Err("a consequence needs a slot such as consequence-mild".into());
            }
            let time = self.valid_from(input.valid_from.as_deref())?;
            let old = if let Some(slot) = &input.slot {
                self.db.query_row(&format!("SELECT {} FROM aspects WHERE holder_id IS ?1 AND scope=?2 AND slot=?3 AND valid_to IS NULL AND superseded_by IS NULL ORDER BY id LIMIT 1",AspectRow::COLUMNS),params![input.holder_id,scope,slot],AspectRow::from_row).optional().map_err(sql_error)?
            } else {
                None
            };
            if old.as_ref().is_some_and(|a| a.valid_from > time) {
                return Err("valid_from is before the aspect it replaces".into());
            }
            let id = self.insert_aspect(
                text,
                scope,
                input.holder_id,
                input.slot.as_deref(),
                &time,
                old.as_ref(),
            )?;
            self.changes.push(format!(
                "aspect #{id}  \"{text}\"  added ({scope}, holder #{}){}",
                input
                    .holder_id
                    .map(|h| h.to_string())
                    .unwrap_or_else(|| "none".into()),
                old.map(|a| format!(" (replaces #{})", a.id))
                    .unwrap_or_default()
            ));
            Ok(id)
        })
    }
    pub(crate) fn insert_aspect(
        &self,
        text: &str,
        scope: &str,
        holder: Option<i64>,
        slot: Option<&str>,
        time: &str,
        old: Option<&AspectRow>,
    ) -> Result<i64, String> {
        self.db.execute("INSERT INTO aspects(text,scope,holder_id,slot,valid_from,begun_event_id,chain_root_id) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![text,scope,holder,slot,time,self.latest_event_id()?,old.and_then(|a|a.chain_root_id)]).map_err(sql_error)?;
        let id = self.db.last_insert_rowid();
        if let Some(a) = old {
            self.db
                .execute(
                    "UPDATE aspects SET valid_to=?1,superseded_by=?2 WHERE id=?3",
                    params![time, id, a.id],
                )
                .map_err(sql_error)?;
        } else {
            self.db
                .execute("UPDATE aspects SET chain_root_id=id WHERE id=?1", [id])
                .map_err(sql_error)?;
        }
        Ok(id)
    }
    /// Renames an aspect by inserting a successor with a contiguous time window.
    pub fn rename_aspect(&mut self, id: i64, new_text: &str) -> Result<i64, String> {
        atomic(self.db, || {
            let old = self.current_aspect(id)?;
            let text = aspect_text(new_text)?;
            if self.now() < old.valid_from.as_str() {
                return Err("valid_from is before the aspect it replaces".into());
            }
            let new = self.insert_aspect(
                text,
                &old.scope,
                old.holder_id,
                old.slot.as_deref(),
                self.now(),
                Some(&old),
            )?;
            self.changes
                .push(format!("aspect #{id} → #{new}  \"{text}\""));
            Ok(new)
        })
    }
    /// Closes an aspect without inserting a successor.
    pub fn expire_aspect(
        &mut self,
        id: i64,
        valid_to: Option<&str>,
        ended_event_id: Option<i64>,
    ) -> Result<(), String> {
        atomic(self.db, || {
            let old = self.current_aspect(id)?;
            let time = valid_to.unwrap_or(self.now());
            check_game_time(time)?;
            if time < old.valid_from.as_str() {
                return Err("valid_to is before the aspect begins".into());
            }
            self.db
                .execute(
                    "UPDATE aspects SET valid_to=?1,ended_event_id=?2 WHERE id=?3",
                    params![time, ended_event_id.or(self.latest_event_id()?), id],
                )
                .map_err(sql_error)?;
            self.changes.push(format!("aspect #{id}  expired"));
            Ok(())
        })
    }
    /// Lists a holder's current aspects; None means campaign aspects.
    pub fn aspects(&self, holder: Option<i64>) -> Result<Vec<AspectRow>, String> {
        rows(
            self.db,
            &format!(
                "SELECT {} FROM aspects WHERE holder_id IS ?1 AND valid_to IS NULL AND superseded_by IS NULL ORDER BY id",
                AspectRow::COLUMNS
            ),
            [holder],
            AspectRow::from_row,
        )
    }
    /// Lists the aspects true at the supplied game time.
    pub fn aspects_at(&self, holder: Option<i64>, time: &str) -> Result<Vec<AspectRow>, String> {
        check_game_time(time)?;
        rows(
            self.db,
            &format!(
                "SELECT {} FROM aspects WHERE holder_id IS ?1 AND valid_from<=?2 AND (valid_to IS NULL OR valid_to>?2) ORDER BY id",
                AspectRow::COLUMNS
            ),
            params![holder, time],
            AspectRow::from_row,
        )
    }
    /// Loads all versions in an aspect's chain.
    pub fn aspect_history(&self, id: i64) -> Result<Vec<AspectRow>, String> {
        let a = self.aspect(id)?;
        rows(
            self.db,
            &format!(
                "SELECT {} FROM aspects WHERE chain_root_id=?1 ORDER BY valid_from,id",
                AspectRow::COLUMNS
            ),
            [a.chain_root_id],
            AspectRow::from_row,
        )
    }
    /// Searches current aspects using escaped full-text keywords.
    pub fn search_aspects(&self, keywords: &str) -> Result<Vec<AspectRow>, String> {
        let query = keywords
            .split(|c: char| !c.is_alphanumeric())
            .filter(|s| !s.is_empty())
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(" OR ");
        if query.is_empty() {
            return Ok(vec![]);
        }
        let columns = AspectRow::COLUMNS
            .split(", ")
            .map(|c| format!("a.{c}"))
            .collect::<Vec<_>>()
            .join(", ");
        rows(
            self.db,
            &format!(
                "SELECT {columns} FROM aspects_fts JOIN aspects a ON a.id=aspects_fts.rowid WHERE aspects_fts MATCH ?1 AND a.valid_to IS NULL AND a.superseded_by IS NULL ORDER BY a.id"
            ),
            [query],
            AspectRow::from_row,
        )
    }
}
impl World<'_> {
    fn finish_version(
        &self,
        table: &str,
        id: i64,
        old: Option<(i64, i64)>,
        time: &str,
    ) -> Result<(), String> {
        if let Some((prior, _)) = old {
            self.db
                .execute(
                    &format!("UPDATE {table} SET valid_to=?1,superseded_by=?2 WHERE id=?3"),
                    params![time, id, prior],
                )
                .map_err(sql_error)?;
        } else {
            self.db
                .execute(
                    &format!("UPDATE {table} SET chain_root_id=id WHERE id=?1"),
                    [id],
                )
                .map_err(sql_error)?;
        }
        Ok(())
    }
    /// Inserts a relationship version, replacing the current edge of the same type.
    pub fn update_relationship(
        &mut self,
        from: i64,
        to: i64,
        kind: &str,
        score: i64,
        valid_from: Option<&str>,
    ) -> Result<i64, String> {
        atomic(self.db, || {
            let a = self.entity(from)?;
            self.entity(to)?;
            let kind = text_limit(kind, "relationship type", 40)?;
            if !(-100..=100).contains(&score) {
                return Err("score must be between -100 and 100".into());
            }
            let time = self.valid_from(valid_from)?;
            let old = self
                .relationships(from, Some(to))?
                .into_iter()
                .find(|r| r.kind == kind);
            if old.as_ref().is_some_and(|r| r.valid_from > time) {
                return Err("valid_from is before the relationship it replaces".into());
            }
            self.db.execute("INSERT INTO relationships(from_entity_id,to_entity_id,type,score,valid_from,chain_root_id) VALUES(?1,?2,?3,?4,?5,?6)",params![from,to,kind,score,time,old.as_ref().and_then(|r|r.chain_root_id)]).map_err(sql_error)?;
            let id = self.db.last_insert_rowid();
            self.finish_version(
                "relationships",
                id,
                old.as_ref()
                    .map(|r| (r.id, r.chain_root_id.unwrap_or(r.id))),
                &time,
            )?;
            self.changes.push(format!(
                "relationship  {}→{}  {kind}  score {score}{}",
                a.name,
                self.entity(to)?.name,
                old.map(|r| format!("  (was {})", r.score))
                    .unwrap_or_default()
            ));
            Ok(id)
        })
    }
    /// Lists current outgoing relationships, optionally to one entity.
    pub fn relationships(
        &self,
        from: i64,
        to: Option<i64>,
    ) -> Result<Vec<RelationshipRow>, String> {
        self.entity(from)?;
        if let Some(to) = to {
            self.entity(to)?;
        }
        rows(
            self.db,
            &format!(
                "SELECT {} FROM relationships WHERE from_entity_id=?1 AND (?2 IS NULL OR to_entity_id=?2) AND valid_to IS NULL AND superseded_by IS NULL ORDER BY id",
                RelationshipRow::COLUMNS
            ),
            params![from, to],
            RelationshipRow::from_row,
        )
    }
    /// Sets a skill or approach on the Fate ladder by adding a version.
    pub fn set_skill(
        &mut self,
        entity_id: i64,
        name: &str,
        rank: i64,
        kind: Option<&str>,
        valid_from: Option<&str>,
    ) -> Result<i64, String> {
        atomic(self.db, || {
            let entity = self.entity(entity_id)?;
            let name = text_limit(name, "skill name", 40)?;
            if !(0..=8).contains(&rank) {
                return Err("rank must be 0 to 8 on the Fate ladder".into());
            }
            let kind = kind.unwrap_or("skill");
            if !["skill", "approach"].contains(&kind) {
                return Err("kind must be skill or approach".into());
            }
            let time = self.valid_from(valid_from)?;
            let old = self.skills(entity_id)?.into_iter().find(|r| r.name == name);
            if old.as_ref().is_some_and(|r| r.valid_from > time) {
                return Err("valid_from is before the skill it replaces".into());
            }
            self.db.execute("INSERT INTO skills(entity_id,name,rank,kind,valid_from,chain_root_id) VALUES(?1,?2,?3,?4,?5,?6)",params![entity_id,name,rank,kind,time,old.as_ref().and_then(|r|r.chain_root_id)]).map_err(sql_error)?;
            let id = self.db.last_insert_rowid();
            self.finish_version(
                "skills",
                id,
                old.map(|r| (r.id, r.chain_root_id.unwrap_or(r.id))),
                &time,
            )?;
            self.changes
                .push(format!("skill  {} {name} +{rank}", entity.name));
            Ok(id)
        })
    }
    /// Lists an entity's current skills.
    pub fn skills(&self, id: i64) -> Result<Vec<SkillRow>, String> {
        self.entity(id)?;
        rows(
            self.db,
            &format!(
                "SELECT {} FROM skills WHERE entity_id=?1 AND valid_to IS NULL AND superseded_by IS NULL ORDER BY id",
                SkillRow::COLUMNS
            ),
            [id],
            SkillRow::from_row,
        )
    }
    /// Adds a stunt or disposition with a trigger and action.
    pub fn add_behavior(
        &mut self,
        id: i64,
        kind: &str,
        trigger: &str,
        action: &str,
        priority: i64,
    ) -> Result<i64, String> {
        atomic(self.db, || {
            let entity = self.entity(id)?;
            if !["stunt", "disposition"].contains(&kind) {
                return Err("kind must be stunt or disposition".into());
            }
            let trigger = text_limit(trigger, "trigger", 120)?;
            let action = text_limit(action, "action", 120)?;
            self.db.execute("INSERT INTO behaviors(entity_id,kind,trigger,action,priority,valid_from) VALUES(?1,?2,?3,?4,?5,?6)",params![id,kind,trigger,action,priority,self.now()]).map_err(sql_error)?;
            let new = self.db.last_insert_rowid();
            self.finish_version("behaviors", new, None, self.now())?;
            self.changes.push(format!(
                "behavior #{new}  {}: {trigger} → {action}",
                entity.name
            ));
            Ok(new)
        })
    }
    /// Lists current behaviors in insertion order.
    pub fn behaviors(&self, id: i64) -> Result<Vec<BehaviorRow>, String> {
        self.entity(id)?;
        rows(
            self.db,
            &format!(
                "SELECT {} FROM behaviors WHERE entity_id=?1 AND valid_to IS NULL AND superseded_by IS NULL ORDER BY id",
                BehaviorRow::COLUMNS
            ),
            [id],
            BehaviorRow::from_row,
        )
    }
    /// Adds a uniquely named stress track with one to ten boxes.
    pub fn add_stress_track(&mut self, id: i64, name: &str, size: i64) -> Result<i64, String> {
        atomic(self.db, || {
            let entity = self.entity(id)?;
            let name = text_limit(name, "stress track name", 40)?;
            if !(1..=10).contains(&size) {
                return Err("stress track size must be 1 to 10".into());
            }
            if self
                .stress(id)?
                .iter()
                .any(|r| r.name.eq_ignore_ascii_case(name))
            {
                return Err(format!("{} already has a {name} track", entity.name));
            }
            self.db
                .execute(
                    "INSERT INTO stress_tracks(entity_id,name,size,valid_from) VALUES(?1,?2,?3,?4)",
                    params![id, name, size, self.now()],
                )
                .map_err(sql_error)?;
            let new = self.db.last_insert_rowid();
            self.finish_version("stress_tracks", new, None, self.now())?;
            self.changes.push(format!(
                "stress track #{new}  {} {name} 0/{size}",
                entity.name
            ));
            Ok(new)
        })
    }
    /// Lists current stress tracks.
    pub fn stress(&self, id: i64) -> Result<Vec<StressTrackRow>, String> {
        self.entity(id)?;
        rows(
            self.db,
            &format!(
                "SELECT {} FROM stress_tracks WHERE entity_id=?1 AND valid_to IS NULL AND superseded_by IS NULL ORDER BY id",
                StressTrackRow::COLUMNS
            ),
            [id],
            StressTrackRow::from_row,
        )
    }
    fn change_stress(
        &mut self,
        id: i64,
        track: &str,
        delta: i64,
        clear: bool,
    ) -> Result<i64, String> {
        atomic(self.db, || {
            let entity = self.entity(id)?;
            if delta < 1 {
                return Err("stress delta must be at least 1".into());
            }
            let row = self
                .stress(id)?
                .into_iter()
                .find(|r| r.name.eq_ignore_ascii_case(track))
                .ok_or_else(|| format!("{} has no {track} stress track", entity.name))?;
            if !clear && delta > row.size - row.marked {
                return Err(format!(
                    "the {track} track has only {} free boxes; take a consequence instead",
                    row.size - row.marked
                ));
            }
            let marked = if clear {
                row.marked.saturating_sub(delta).max(0)
            } else {
                row.marked + delta
            };
            self.db
                .execute(
                    "UPDATE stress_tracks SET marked=?1 WHERE id=?2",
                    params![marked, row.id],
                )
                .map_err(sql_error)?;
            self.changes.push(format!(
                "stress  {} {track} {marked}/{}",
                entity.name, row.size
            ));
            Ok(marked)
        })
    }
    /// Marks boxes, refusing overflow so the character can take a consequence.
    pub fn mark_stress(&mut self, id: i64, track: &str, delta: i64) -> Result<i64, String> {
        self.change_stress(id, track, delta, false)
    }
    /// Clears boxes without going below zero.
    pub fn clear_stress(&mut self, id: i64, track: &str, delta: i64) -> Result<i64, String> {
        self.change_stress(id, track, delta, true)
    }
    /// Derives fate points from the transaction ledger.
    pub fn fate_balance(&self, id: i64) -> Result<i64, String> {
        self.entity(id)?;
        self.db
            .query_row(
                "SELECT coalesce(sum(delta),0) FROM fate_transactions WHERE entity_id=?1",
                [id],
                |r| r.get(0),
            )
            .map_err(sql_error)
    }
    fn change_fate(
        &mut self,
        id: i64,
        amount: i64,
        reason: &str,
        aspect: Option<i64>,
        spend: bool,
    ) -> Result<i64, String> {
        atomic(self.db, || {
            let entity = self.entity(id)?;
            if entity.kind != "character" {
                return Err("only characters have fate points".into());
            }
            if amount < 1 {
                return Err("fate amount must be at least 1".into());
            }
            let reason = reason.trim();
            if reason.is_empty() {
                return Err("fate reason is empty".into());
            }
            if let Some(a) = aspect {
                self.aspect(a)?;
            }
            let balance = self.fate_balance(id)?;
            if spend && balance < amount {
                return Err(format!("{} has only {balance} fate points", entity.name));
            }
            let delta = if spend { -amount } else { amount };
            let result = balance
                .checked_add(delta)
                .ok_or("fate balance is too large")?;
            self.db.execute("INSERT INTO fate_transactions(entity_id,delta,reason,aspect_id,event_id,game_time) VALUES(?1,?2,?3,?4,?5,?6)",params![id,delta,reason,aspect,self.latest_event_id()?,self.now()]).map_err(sql_error)?;
            self.changes
                .push(format!("fate  {}  {delta:+}  {reason}", entity.name));
            Ok(result)
        })
    }
    /// Spends available fate points and appends a ledger entry.
    pub fn spend_fate(
        &mut self,
        id: i64,
        amount: i64,
        reason: &str,
        aspect: Option<i64>,
    ) -> Result<i64, String> {
        self.change_fate(id, amount, reason, aspect, true)
    }
    /// Awards fate points and appends a ledger entry.
    pub fn award_fate(
        &mut self,
        id: i64,
        amount: i64,
        reason: &str,
        aspect: Option<i64>,
    ) -> Result<i64, String> {
        self.change_fate(id, amount, reason, aspect, false)
    }
    /// Bundles an entity's current game statistics.
    pub fn entity_record(&self, id: i64) -> Result<serde_json::Value, String> {
        let entity = self.entity(id)?;
        let character = entity.kind == "character";
        let mut value = serde_json::json!({"entity":entity,"aspects":self.aspects(Some(id))?,"skills":self.skills(id)?,"stress":self.stress(id)?,"behaviors":self.behaviors(id)?});
        if character {
            value["fate_points"] = self.fate_balance(id)?.into();
        }
        Ok(value)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    pub fn world_db() -> (rusqlite::Connection, Meta) {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        super::super::schema::migrate(&db).unwrap();
        let meta = Meta {
            name: "Test".into(),
            created: "20261111-000000".into(),
            turn: 0,
            game_time: super::super::store::START_TIME.into(),
            current_scene_id: None,
            roles: Default::default(),
        };
        (db, meta)
    }
    fn input(text: &str, holder: i64) -> AspectInput {
        AspectInput {
            text: text.into(),
            scope: Scope::Character,
            holder_id: Some(holder),
            slot: None,
            valid_from: None,
        }
    }
    #[test]
    fn a_panic_inside_atomic_leaves_the_db_unchanged() {
        let (db, _) = world_db();
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| atomic::<()>(&db, || {
                db.execute(
                    "INSERT INTO entities(name,type) VALUES ('x','character')",
                    [],
                )
                .unwrap();
                panic!("boom")
            })))
            .is_err()
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM entities", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        atomic(&db, || Ok(())).unwrap();
    }
    #[test]
    fn an_error_inside_atomic_rolls_back() {
        let (db, _) = world_db();
        assert!(
            atomic::<()>(&db, || {
                db.execute(
                    "INSERT INTO entities(name,type) VALUES ('x','character')",
                    [],
                )
                .unwrap();
                Err("bad".into())
            })
            .is_err()
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM entities", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn game_time_format_is_checked() {
        check_game_time("1471-03-12T20:00").unwrap();
        for s in [
            "1471-3-12T20:00",
            "1471-03-12 20:00",
            "1471-13-01T00:00",
            "1471-03-12T24:00",
            "",
        ] {
            assert!(check_game_time(s).is_err());
        }
    }
    #[test]
    fn log_event_advances_the_clock_only_forward() {
        let (db, mut meta) = world_db();
        let mut w = World {
            db: &db,
            meta: &mut meta,
            changes: vec![],
        };
        w.log_event("1471-03-12T21:00", "Later", None).unwrap();
        w.log_event("1471-03-12T19:00", "Earlier", None).unwrap();
        assert_eq!(w.now(), "1471-03-12T21:00");
        assert_eq!(
            db.query_row("SELECT max(turn) FROM events", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    #[test]
    fn rename_gives_contiguous_windows_and_a_shared_chain() {
        let (db, mut meta) = world_db();
        let mut w = World {
            db: &db,
            meta: &mut meta,
            changes: vec![],
        };
        let e = w.add_entity("Bridge", "location", None).unwrap();
        let a = w.add_aspect(input("On Fire", e)).unwrap();
        w.log_event("1471-03-12T21:00", "Time", None).unwrap();
        let b = w.rename_aspect(a, "Smouldering Ruins").unwrap();
        let h = w.aspect_history(b).unwrap();
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].valid_to.as_ref(), Some(&h[1].valid_from));
        assert_eq!(h[0].superseded_by, Some(b));
        assert_eq!(h[0].chain_root_id, Some(a));
        assert_eq!(h[1].chain_root_id, Some(a));
    }
    #[test]
    fn a_slotted_aspect_supersedes_the_prior_one() {
        let (db, mut meta) = world_db();
        let mut w = World {
            db: &db,
            meta: &mut meta,
            changes: vec![],
        };
        let e = w.add_entity("Gareth", "character", None).unwrap();
        let mut a = input("Angry", e);
        a.slot = Some("trouble".into());
        let old = w.add_aspect(a.clone()).unwrap();
        a.text = "Furious".into();
        w.add_aspect(a).unwrap();
        w.add_aspect(input("Brave", e)).unwrap();
        w.add_aspect(input("Bold", e)).unwrap();
        assert_eq!(w.aspects(Some(e)).unwrap().len(), 3);
        assert!(w.aspect_history(old).unwrap()[0].valid_to.is_some());
    }
    #[test]
    fn point_in_time_returns_exactly_the_version_true_then() {
        let (db, mut meta) = world_db();
        let mut w = World {
            db: &db,
            meta: &mut meta,
            changes: vec![],
        };
        let e = w.add_entity("Gareth", "character", None).unwrap();
        let a = w.add_aspect(input("v1", e)).unwrap();
        w.log_event("1471-03-12T21:00", "time", None).unwrap();
        let b = w.rename_aspect(a, "v2").unwrap();
        w.log_event("1471-03-12T22:00", "time", None).unwrap();
        w.rename_aspect(b, "v3").unwrap();
        for (time, text) in [("20:30", "v1"), ("21:00", "v2"), ("23:00", "v3")] {
            assert_eq!(
                w.aspects_at(Some(e), &format!("1471-03-12T{time}"))
                    .unwrap()[0]
                    .text,
                text
            );
        }
        assert!(
            w.aspects_at(Some(e), "1471-03-12T19:00")
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn validation_rejects_bad_aspects() {
        let (db, mut meta) = world_db();
        let mut w = World {
            db: &db,
            meta: &mut meta,
            changes: vec![],
        };
        let e = w.add_entity("Gareth", "character", None).unwrap();
        for (a, expected) in [
            (input("", e), String::from("aspect text is empty")),
            (
                input(&"x".repeat(81), e),
                "aspect text is over 80 characters; an aspect is a short phrase".into(),
            ),
            (
                AspectInput {
                    scope: Scope::Campaign,
                    ..input("a", e)
                },
                "a campaign aspect has no holder".into(),
            ),
            (
                AspectInput {
                    scope: Scope::Situation,
                    holder_id: None,
                    ..input("a", e)
                },
                "a situation aspect needs a holder_id".into(),
            ),
            (input("a", 999), "no entity #999".into()),
            (
                AspectInput {
                    scope: Scope::Consequence,
                    ..input("a", e)
                },
                "a consequence needs a slot such as consequence-mild".into(),
            ),
            (
                AspectInput {
                    valid_from: Some("1471-03-12T19:00".into()),
                    ..input("a", e)
                },
                "valid_from 1471-03-12T19:00 is before the current game time 1471-03-12T20:00"
                    .into(),
            ),
        ] {
            assert_eq!(w.add_aspect(a).unwrap_err(), expected);
        }
        assert!(w.aspects(Some(e)).unwrap().is_empty());
    }
    #[test]
    fn expire_closes_without_a_successor() {
        let (db, mut meta) = world_db();
        let mut w = World {
            db: &db,
            meta: &mut meta,
            changes: vec![],
        };
        let e = w.add_entity("Gareth", "character", None).unwrap();
        let a = w.add_aspect(input("Angry", e)).unwrap();
        w.expire_aspect(a, None, None).unwrap();
        assert!(w.aspects(Some(e)).unwrap().is_empty());
        let h = w.aspect_history(a).unwrap();
        assert!(h[0].valid_to.is_some());
        assert!(h[0].superseded_by.is_none());
    }
    #[test]
    fn search_finds_current_aspects_by_keyword() {
        let (db, mut meta) = world_db();
        let mut w = World {
            db: &db,
            meta: &mut meta,
            changes: vec![],
        };
        let e = w.add_entity("Roof", "location", None).unwrap();
        let a = w.add_aspect(input("Rain-slicked Rooftops", e)).unwrap();
        assert_eq!(w.search_aspects("rooftops!").unwrap().len(), 1);
        w.rename_aspect(a, "Dry Stones").unwrap();
        assert!(w.search_aspects("rooftops!").unwrap().is_empty());
        assert!(w.search_aspects("   ").unwrap().is_empty());
    }

    #[test]
    fn update_relationship_supersedes_the_same_edge_type() {
        let (db, mut m) = world_db();
        let mut w = World {
            db: &db,
            meta: &mut m,
            changes: vec![],
        };
        let a = w.add_entity("A", "character", None).unwrap();
        let b = w.add_entity("B", "character", None).unwrap();
        let old = w.update_relationship(a, b, "ally", 10, None).unwrap();
        let new = w.update_relationship(a, b, "ally", 40, None).unwrap();
        w.update_relationship(a, b, "owes", 5, None).unwrap();
        let r = w.relationships(a, Some(b)).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].id, new);
        assert_eq!(r[0].chain_root_id, Some(old));
        assert_eq!(
            db.query_row(
                "SELECT superseded_by FROM relationships WHERE id=?1",
                [old],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            new
        );
    }
    #[test]
    fn relationship_score_is_bounded() {
        let (db, mut m) = world_db();
        let mut w = World {
            db: &db,
            meta: &mut m,
            changes: vec![],
        };
        let a = w.add_entity("A", "character", None).unwrap();
        for n in [-101, 101] {
            assert!(w.update_relationship(a, a, "ally", n, None).is_err());
        }
    }
    #[test]
    fn set_skill_supersedes_and_checks_rank() {
        let (db, mut m) = world_db();
        let mut w = World {
            db: &db,
            meta: &mut m,
            changes: vec![],
        };
        let a = w.add_entity("A", "character", None).unwrap();
        let first = w.set_skill(a, "Fight", 2, None, None).unwrap();
        w.set_skill(a, "Fight", 3, None, None).unwrap();
        let skills = w.skills(a).unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].rank, 3);
        assert_eq!(skills[0].chain_root_id, Some(first));
        for n in [-1, 9] {
            assert!(w.set_skill(a, "Fight", n, None, None).is_err());
        }
    }
    #[test]
    fn stress_marks_and_clears_within_the_track() {
        let (db, mut m) = world_db();
        let mut w = World {
            db: &db,
            meta: &mut m,
            changes: vec![],
        };
        let a = w.add_entity("A", "character", None).unwrap();
        w.add_stress_track(a, "Physical", 3).unwrap();
        assert_eq!(w.mark_stress(a, "physical", 2).unwrap(), 2);
        assert!(w.mark_stress(a, "physical", 2).is_err());
        assert_eq!(w.stress(a).unwrap()[0].marked, 2);
        assert_eq!(w.clear_stress(a, "Physical", 5).unwrap(), 0);
    }
    #[test]
    fn fate_balance_is_the_sum_of_transactions() {
        let (db, mut m) = world_db();
        let mut w = World {
            db: &db,
            meta: &mut m,
            changes: vec![],
        };
        let a = w.add_entity("A", "character", None).unwrap();
        assert_eq!(w.award_fate(a, 3, "refresh", None).unwrap(), 3);
        assert_eq!(w.spend_fate(a, 1, "invoke", None).unwrap(), 2);
        assert!(w.spend_fate(a, 3, "invoke", None).is_err());
        let l = w.add_entity("Bridge", "location", None).unwrap();
        assert!(w.award_fate(l, 1, "refresh", None).is_err());
    }
    #[test]
    fn a_storm_is_a_character_too() {
        let (db, mut m) = world_db();
        let mut w = World {
            db: &db,
            meta: &mut m,
            changes: vec![],
        };
        let a = w.add_entity("Storm", "storm", None).unwrap();
        w.add_aspect(input("Furious", a)).unwrap();
        w.set_skill(a, "Blow", 3, None, None).unwrap();
        w.add_stress_track(a, "Power", 3).unwrap();
        w.update_relationship(a, a, "growing", 5, None).unwrap();
        let r = w.entity_record(a).unwrap();
        assert_eq!(r["aspects"].as_array().unwrap().len(), 1);
        assert_eq!(r["skills"].as_array().unwrap().len(), 1);
        assert_eq!(r["stress"].as_array().unwrap().len(), 1);
        assert_eq!(w.relationships(a, None).unwrap().len(), 1);
        assert!(r.get("fate_points").is_none());
    }
}

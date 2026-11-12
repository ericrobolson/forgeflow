//! SQLite migrations and serializable world rows.
/// Forward-only migrations; migration N brings user_version to N.
pub const MIGRATIONS: &[&str] = &[include_str!("schema.sql")];
/// Formats SQLite failures for the user.
pub fn sql_error(error: rusqlite::Error) -> String {
    format!("database: {error}")
}
/// Enables WAL and NORMAL synchronization on a world connection.
pub fn configure(db: &rusqlite::Connection) -> Result<(), String> {
    db.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0))
        .map_err(sql_error)?;
    db.execute_batch("PRAGMA synchronous=NORMAL")
        .map_err(sql_error)
}
/// Applies pending migrations, each in its own transaction.
pub fn migrate(db: &rusqlite::Connection) -> Result<usize, String> {
    let version: i64 = db
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(sql_error)?;
    if version < 0 || version as usize > MIGRATIONS.len() {
        return Err(format!("unsupported database version {version}"));
    }
    for (i, migration) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        if let Err(e) = db.execute_batch(&format!(
            "BEGIN; {migration} PRAGMA user_version = {}; COMMIT;",
            i + 1
        )) {
            let _ = db.execute_batch("ROLLBACK");
            return Err(format!("migration {}: {e}", i + 1));
        }
    }
    Ok(MIGRATIONS.len() - version as usize)
}
/// A row from the events table.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EventRow {
    pub id: i64,
    pub turn: i64,
    pub game_time: String,
    pub actor_entity_id: Option<i64>,
    pub summary: String,
}
impl EventRow {
    /// Columns selected by from_row, in their field order.
    pub const COLUMNS: &'static str = "id, turn, game_time, actor_entity_id, summary";
    /// Reads a row selected with COLUMNS.
    pub fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            turn: row.get(1)?,
            game_time: row.get(2)?,
            actor_entity_id: row.get(3)?,
            summary: row.get(4)?,
        })
    }
}
/// A row from the entities table.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EntityRow {
    pub id: i64,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub attributes: Option<String>,
    pub created_event_id: Option<i64>,
}
impl EntityRow {
    /// Columns selected by from_row, in their field order.
    pub const COLUMNS: &'static str = "id, name, type, attributes, created_event_id";
    /// Reads a row selected with COLUMNS.
    pub fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            name: row.get(1)?,
            kind: row.get(2)?,
            attributes: row.get(3)?,
            created_event_id: row.get(4)?,
        })
    }
}
/// A row from the aspects table.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AspectRow {
    pub id: i64,
    pub text: String,
    pub scope: String,
    pub holder_id: Option<i64>,
    pub slot: Option<String>,
    pub valid_from: String,
    pub valid_to: Option<String>,
    pub begun_event_id: Option<i64>,
    pub ended_event_id: Option<i64>,
    pub superseded_by: Option<i64>,
    pub chain_root_id: Option<i64>,
}
impl AspectRow {
    /// Columns selected by from_row, in their field order.
    pub const COLUMNS: &'static str = "id, text, scope, holder_id, slot, valid_from, valid_to, begun_event_id, ended_event_id, superseded_by, chain_root_id";
    /// Reads a row selected with COLUMNS.
    pub fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            text: row.get(1)?,
            scope: row.get(2)?,
            holder_id: row.get(3)?,
            slot: row.get(4)?,
            valid_from: row.get(5)?,
            valid_to: row.get(6)?,
            begun_event_id: row.get(7)?,
            ended_event_id: row.get(8)?,
            superseded_by: row.get(9)?,
            chain_root_id: row.get(10)?,
        })
    }
}
/// A row from the skills table.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SkillRow {
    pub id: i64,
    pub entity_id: i64,
    pub name: String,
    pub rank: i64,
    pub kind: String,
    pub valid_from: String,
    pub valid_to: Option<String>,
    pub superseded_by: Option<i64>,
    pub chain_root_id: Option<i64>,
}
impl SkillRow {
    /// Columns selected by from_row, in their field order.
    pub const COLUMNS: &'static str =
        "id, entity_id, name, rank, kind, valid_from, valid_to, superseded_by, chain_root_id";
    /// Reads a row selected with COLUMNS.
    pub fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            entity_id: row.get(1)?,
            name: row.get(2)?,
            rank: row.get(3)?,
            kind: row.get(4)?,
            valid_from: row.get(5)?,
            valid_to: row.get(6)?,
            superseded_by: row.get(7)?,
            chain_root_id: row.get(8)?,
        })
    }
}
/// A row from the behaviors table.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BehaviorRow {
    pub id: i64,
    pub entity_id: i64,
    pub kind: String,
    pub trigger: String,
    pub action: String,
    pub priority: i64,
    pub valid_from: String,
    pub valid_to: Option<String>,
    pub superseded_by: Option<i64>,
    pub chain_root_id: Option<i64>,
}
impl BehaviorRow {
    /// Columns selected by from_row, in their field order.
    pub const COLUMNS: &'static str = "id, entity_id, kind, trigger, action, priority, valid_from, valid_to, superseded_by, chain_root_id";
    /// Reads a row selected with COLUMNS.
    pub fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            entity_id: row.get(1)?,
            kind: row.get(2)?,
            trigger: row.get(3)?,
            action: row.get(4)?,
            priority: row.get(5)?,
            valid_from: row.get(6)?,
            valid_to: row.get(7)?,
            superseded_by: row.get(8)?,
            chain_root_id: row.get(9)?,
        })
    }
}
/// A row from the stress_tracks table.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StressTrackRow {
    pub id: i64,
    pub entity_id: i64,
    pub name: String,
    pub size: i64,
    pub marked: i64,
    pub valid_from: String,
    pub valid_to: Option<String>,
    pub superseded_by: Option<i64>,
    pub chain_root_id: Option<i64>,
}
impl StressTrackRow {
    /// Columns selected by from_row, in their field order.
    pub const COLUMNS: &'static str =
        "id, entity_id, name, size, marked, valid_from, valid_to, superseded_by, chain_root_id";
    /// Reads a row selected with COLUMNS.
    pub fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            entity_id: row.get(1)?,
            name: row.get(2)?,
            size: row.get(3)?,
            marked: row.get(4)?,
            valid_from: row.get(5)?,
            valid_to: row.get(6)?,
            superseded_by: row.get(7)?,
            chain_root_id: row.get(8)?,
        })
    }
}
/// A row from the relationships table.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RelationshipRow {
    pub id: i64,
    pub from_entity_id: i64,
    pub to_entity_id: i64,
    #[serde(rename = "type")]
    pub kind: String,
    pub score: i64,
    pub valid_from: String,
    pub valid_to: Option<String>,
    pub superseded_by: Option<i64>,
    pub chain_root_id: Option<i64>,
}
impl RelationshipRow {
    /// Columns selected by from_row, in their field order.
    pub const COLUMNS: &'static str = "id, from_entity_id, to_entity_id, type, score, valid_from, valid_to, superseded_by, chain_root_id";
    /// Reads a row selected with COLUMNS.
    pub fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            from_entity_id: row.get(1)?,
            to_entity_id: row.get(2)?,
            kind: row.get(3)?,
            score: row.get(4)?,
            valid_from: row.get(5)?,
            valid_to: row.get(6)?,
            superseded_by: row.get(7)?,
            chain_root_id: row.get(8)?,
        })
    }
}
/// A row from the scenes table.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SceneRow {
    pub id: i64,
    pub title: String,
    pub location_entity_id: Option<i64>,
    pub chapter: i64,
    pub summary: Option<String>,
    pub started_event_id: Option<i64>,
    pub ended_event_id: Option<i64>,
    pub valid_from: String,
    pub valid_to: Option<String>,
    pub superseded_by: Option<i64>,
    pub chain_root_id: Option<i64>,
}
impl SceneRow {
    /// Columns selected by from_row, in their field order.
    pub const COLUMNS: &'static str = "id, title, location_entity_id, chapter, summary, started_event_id, ended_event_id, valid_from, valid_to, superseded_by, chain_root_id";
    /// Reads a row selected with COLUMNS.
    pub fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            title: row.get(1)?,
            location_entity_id: row.get(2)?,
            chapter: row.get(3)?,
            summary: row.get(4)?,
            started_event_id: row.get(5)?,
            ended_event_id: row.get(6)?,
            valid_from: row.get(7)?,
            valid_to: row.get(8)?,
            superseded_by: row.get(9)?,
            chain_root_id: row.get(10)?,
        })
    }
}
/// A row from the presence table.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PresenceRow {
    pub id: i64,
    pub scene_id: i64,
    pub entity_id: i64,
    pub role: String,
    pub valid_from: String,
    pub valid_to: Option<String>,
    pub joined_event_id: Option<i64>,
    pub left_event_id: Option<i64>,
    pub superseded_by: Option<i64>,
    pub chain_root_id: Option<i64>,
}
impl PresenceRow {
    /// Columns selected by from_row, in their field order.
    pub const COLUMNS: &'static str = "id, scene_id, entity_id, role, valid_from, valid_to, joined_event_id, left_event_id, superseded_by, chain_root_id";
    /// Reads a row selected with COLUMNS.
    pub fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            scene_id: row.get(1)?,
            entity_id: row.get(2)?,
            role: row.get(3)?,
            valid_from: row.get(4)?,
            valid_to: row.get(5)?,
            joined_event_id: row.get(6)?,
            left_event_id: row.get(7)?,
            superseded_by: row.get(8)?,
            chain_root_id: row.get(9)?,
        })
    }
}
/// A row from the fate_transactions table.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FateRow {
    pub id: i64,
    pub entity_id: i64,
    pub delta: i64,
    pub reason: Option<String>,
    pub aspect_id: Option<i64>,
    pub event_id: Option<i64>,
    pub game_time: String,
}
impl FateRow {
    /// Columns selected by from_row, in their field order.
    pub const COLUMNS: &'static str =
        "id, entity_id, delta, reason, aspect_id, event_id, game_time";
    /// Reads a row selected with COLUMNS.
    pub fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            entity_id: row.get(1)?,
            delta: row.get(2)?,
            reason: row.get(3)?,
            aspect_id: row.get(4)?,
            event_id: row.get(5)?,
            game_time: row.get(6)?,
        })
    }
}
/// A row from the free_invokes table.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FreeInvokeRow {
    pub aspect_id: i64,
    pub beneficiary_entity_id: Option<i64>,
    pub count: i64,
}
impl FreeInvokeRow {
    /// Columns selected by from_row, in their field order.
    pub const COLUMNS: &'static str = "aspect_id, beneficiary_entity_id, count";
    /// Reads a row selected with COLUMNS.
    pub fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            aspect_id: row.get(0)?,
            beneficiary_entity_id: row.get(1)?,
            count: row.get(2)?,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migrate_creates_every_table_and_sets_the_version() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        configure(&db).unwrap();
        assert_eq!(migrate(&db).unwrap(), 1);
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        for table in [
            "events",
            "entities",
            "aspects",
            "skills",
            "behaviors",
            "stress_tracks",
            "relationships",
            "scenes",
            "presence",
            "fate_transactions",
            "free_invokes",
            "aspects_fts",
        ] {
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name=?1",
                    [table],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                1
            );
        }
    }
    #[test]
    fn migrate_is_idempotent() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        migrate(&db).unwrap();
        assert_eq!(migrate(&db).unwrap(), 0);
    }
    #[test]
    fn fts_finds_inserted_aspect_text() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        migrate(&db).unwrap();
        db.execute("INSERT INTO aspects(text,scope,valid_from) VALUES ('Rain-slicked Rooftops','campaign','1471-03-12T20:00')", []).unwrap();
        assert_eq!(
            db.query_row(
                r#"SELECT rowid FROM aspects_fts WHERE aspects_fts MATCH '"rooftops"'"#,
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }
    #[test]
    fn every_row_type_round_trips() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        migrate(&db).unwrap();
        db.execute("INSERT INTO events (id, turn, game_time, actor_entity_id, summary) VALUES (1, 1, 'example', 1, 'example')", []).unwrap();
        let expected = EventRow {
            id: 1,
            turn: 1,
            game_time: "example".into(),
            actor_entity_id: Some(1),
            summary: "example".into(),
        };
        let row = db
            .query_row(
                &format!("SELECT {} FROM events", EventRow::COLUMNS),
                [],
                EventRow::from_row,
            )
            .unwrap();
        assert_eq!(row, expected);
        assert_eq!(
            serde_json::from_str::<EventRow>(&serde_json::to_string(&row).unwrap()).unwrap(),
            expected
        );
        db.execute("INSERT INTO entities (id, name, type, attributes, created_event_id) VALUES (1, 'example', 'example', 'example', 1)", []).unwrap();
        let expected = EntityRow {
            id: 1,
            name: "example".into(),
            kind: "example".into(),
            attributes: Some("example".into()),
            created_event_id: Some(1),
        };
        let row = db
            .query_row(
                &format!("SELECT {} FROM entities", EntityRow::COLUMNS),
                [],
                EntityRow::from_row,
            )
            .unwrap();
        assert_eq!(row, expected);
        assert_eq!(
            serde_json::from_str::<EntityRow>(&serde_json::to_string(&row).unwrap()).unwrap(),
            expected
        );
        db.execute("INSERT INTO aspects (id, text, scope, holder_id, slot, valid_from, valid_to, begun_event_id, ended_event_id, superseded_by, chain_root_id) VALUES (1, 'example', 'example', 1, 'example', 'example', 'example', 1, 1, 1, 1)", []).unwrap();
        let expected = AspectRow {
            id: 1,
            text: "example".into(),
            scope: "example".into(),
            holder_id: Some(1),
            slot: Some("example".into()),
            valid_from: "example".into(),
            valid_to: Some("example".into()),
            begun_event_id: Some(1),
            ended_event_id: Some(1),
            superseded_by: Some(1),
            chain_root_id: Some(1),
        };
        let row = db
            .query_row(
                &format!("SELECT {} FROM aspects", AspectRow::COLUMNS),
                [],
                AspectRow::from_row,
            )
            .unwrap();
        assert_eq!(row, expected);
        assert_eq!(
            serde_json::from_str::<AspectRow>(&serde_json::to_string(&row).unwrap()).unwrap(),
            expected
        );
        db.execute("INSERT INTO skills (id, entity_id, name, rank, kind, valid_from, valid_to, superseded_by, chain_root_id) VALUES (1, 1, 'example', 1, 'example', 'example', 'example', 1, 1)", []).unwrap();
        let expected = SkillRow {
            id: 1,
            entity_id: 1,
            name: "example".into(),
            rank: 1,
            kind: "example".into(),
            valid_from: "example".into(),
            valid_to: Some("example".into()),
            superseded_by: Some(1),
            chain_root_id: Some(1),
        };
        let row = db
            .query_row(
                &format!("SELECT {} FROM skills", SkillRow::COLUMNS),
                [],
                SkillRow::from_row,
            )
            .unwrap();
        assert_eq!(row, expected);
        assert_eq!(
            serde_json::from_str::<SkillRow>(&serde_json::to_string(&row).unwrap()).unwrap(),
            expected
        );
        db.execute("INSERT INTO behaviors (id, entity_id, kind, trigger, action, priority, valid_from, valid_to, superseded_by, chain_root_id) VALUES (1, 1, 'example', 'example', 'example', 1, 'example', 'example', 1, 1)", []).unwrap();
        let expected = BehaviorRow {
            id: 1,
            entity_id: 1,
            kind: "example".into(),
            trigger: "example".into(),
            action: "example".into(),
            priority: 1,
            valid_from: "example".into(),
            valid_to: Some("example".into()),
            superseded_by: Some(1),
            chain_root_id: Some(1),
        };
        let row = db
            .query_row(
                &format!("SELECT {} FROM behaviors", BehaviorRow::COLUMNS),
                [],
                BehaviorRow::from_row,
            )
            .unwrap();
        assert_eq!(row, expected);
        assert_eq!(
            serde_json::from_str::<BehaviorRow>(&serde_json::to_string(&row).unwrap()).unwrap(),
            expected
        );
        db.execute("INSERT INTO stress_tracks (id, entity_id, name, size, marked, valid_from, valid_to, superseded_by, chain_root_id) VALUES (1, 1, 'example', 1, 1, 'example', 'example', 1, 1)", []).unwrap();
        let expected = StressTrackRow {
            id: 1,
            entity_id: 1,
            name: "example".into(),
            size: 1,
            marked: 1,
            valid_from: "example".into(),
            valid_to: Some("example".into()),
            superseded_by: Some(1),
            chain_root_id: Some(1),
        };
        let row = db
            .query_row(
                &format!("SELECT {} FROM stress_tracks", StressTrackRow::COLUMNS),
                [],
                StressTrackRow::from_row,
            )
            .unwrap();
        assert_eq!(row, expected);
        assert_eq!(
            serde_json::from_str::<StressTrackRow>(&serde_json::to_string(&row).unwrap()).unwrap(),
            expected
        );
        db.execute("INSERT INTO relationships (id, from_entity_id, to_entity_id, type, score, valid_from, valid_to, superseded_by, chain_root_id) VALUES (1, 1, 1, 'example', 1, 'example', 'example', 1, 1)", []).unwrap();
        let expected = RelationshipRow {
            id: 1,
            from_entity_id: 1,
            to_entity_id: 1,
            kind: "example".into(),
            score: 1,
            valid_from: "example".into(),
            valid_to: Some("example".into()),
            superseded_by: Some(1),
            chain_root_id: Some(1),
        };
        let row = db
            .query_row(
                &format!("SELECT {} FROM relationships", RelationshipRow::COLUMNS),
                [],
                RelationshipRow::from_row,
            )
            .unwrap();
        assert_eq!(row, expected);
        assert_eq!(
            serde_json::from_str::<RelationshipRow>(&serde_json::to_string(&row).unwrap()).unwrap(),
            expected
        );
        db.execute("INSERT INTO scenes (id, title, location_entity_id, chapter, summary, started_event_id, ended_event_id, valid_from, valid_to, superseded_by, chain_root_id) VALUES (1, 'example', 1, 1, 'example', 1, 1, 'example', 'example', 1, 1)", []).unwrap();
        let expected = SceneRow {
            id: 1,
            title: "example".into(),
            location_entity_id: Some(1),
            chapter: 1,
            summary: Some("example".into()),
            started_event_id: Some(1),
            ended_event_id: Some(1),
            valid_from: "example".into(),
            valid_to: Some("example".into()),
            superseded_by: Some(1),
            chain_root_id: Some(1),
        };
        let row = db
            .query_row(
                &format!("SELECT {} FROM scenes", SceneRow::COLUMNS),
                [],
                SceneRow::from_row,
            )
            .unwrap();
        assert_eq!(row, expected);
        assert_eq!(
            serde_json::from_str::<SceneRow>(&serde_json::to_string(&row).unwrap()).unwrap(),
            expected
        );
        db.execute("INSERT INTO presence (id, scene_id, entity_id, role, valid_from, valid_to, joined_event_id, left_event_id, superseded_by, chain_root_id) VALUES (1, 1, 1, 'example', 'example', 'example', 1, 1, 1, 1)", []).unwrap();
        let expected = PresenceRow {
            id: 1,
            scene_id: 1,
            entity_id: 1,
            role: "example".into(),
            valid_from: "example".into(),
            valid_to: Some("example".into()),
            joined_event_id: Some(1),
            left_event_id: Some(1),
            superseded_by: Some(1),
            chain_root_id: Some(1),
        };
        let row = db
            .query_row(
                &format!("SELECT {} FROM presence", PresenceRow::COLUMNS),
                [],
                PresenceRow::from_row,
            )
            .unwrap();
        assert_eq!(row, expected);
        assert_eq!(
            serde_json::from_str::<PresenceRow>(&serde_json::to_string(&row).unwrap()).unwrap(),
            expected
        );
        db.execute("INSERT INTO fate_transactions (id, entity_id, delta, reason, aspect_id, event_id, game_time) VALUES (1, 1, 1, 'example', 1, 1, 'example')", []).unwrap();
        let expected = FateRow {
            id: 1,
            entity_id: 1,
            delta: 1,
            reason: Some("example".into()),
            aspect_id: Some(1),
            event_id: Some(1),
            game_time: "example".into(),
        };
        let row = db
            .query_row(
                &format!("SELECT {} FROM fate_transactions", FateRow::COLUMNS),
                [],
                FateRow::from_row,
            )
            .unwrap();
        assert_eq!(row, expected);
        assert_eq!(
            serde_json::from_str::<FateRow>(&serde_json::to_string(&row).unwrap()).unwrap(),
            expected
        );
        db.execute(
            "INSERT INTO free_invokes (aspect_id, beneficiary_entity_id, count) VALUES (1, 1, 1)",
            [],
        )
        .unwrap();
        let expected = FreeInvokeRow {
            aspect_id: 1,
            beneficiary_entity_id: Some(1),
            count: 1,
        };
        let row = db
            .query_row(
                &format!("SELECT {} FROM free_invokes", FreeInvokeRow::COLUMNS),
                [],
                FreeInvokeRow::from_row,
            )
            .unwrap();
        assert_eq!(row, expected);
        assert_eq!(
            serde_json::from_str::<FreeInvokeRow>(&serde_json::to_string(&row).unwrap()).unwrap(),
            expected
        );
    }
}

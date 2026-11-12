//! Persistent RPG sessions, process locks, and SQLite backups.
use super::schema::{configure, migrate, sql_error};
use crate::{project::Project, web_util::utc_stamp};
use std::{
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
/// The session SQLite filename.
pub const DB_FILE: &str = "world.db";
/// The committed session metadata filename.
pub const META_FILE: &str = "meta.json";
/// The reproducible world echo filename.
pub const ECHO_FILE: &str = "events.jsonl";
/// The compact SQLite backup filename.
pub const BACKUP_FILE: &str = "world.vacuum";
/// The exclusive session lock filename.
pub const LOCK_FILE: &str = ".lock";
/// The local turn transcript folder.
pub const TRANSCRIPTS: &str = "transcripts";
/// The initial in-world calendar time.
pub const START_TIME: &str = "1471-03-12T20:00";
/// Turns between automatic backups.
pub const BACKUP_EVERY: i64 = 10;
/// Model and decision roles configured for a session.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RoleConfig {
    pub narrator: String,
    pub archivist: String,
    pub compel_decider: String,
}
impl Default for RoleConfig {
    fn default() -> Self {
        Self {
            narrator: "local".into(),
            archivist: "local".into(),
            compel_decider: "rules".into(),
        }
    }
}
/// The committed session clock and role configuration.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Meta {
    pub name: String,
    pub created: String,
    pub turn: i64,
    pub game_time: String,
    pub current_scene_id: Option<i64>,
    #[serde(default)]
    pub roles: RoleConfig,
}
/// A menu entry read without opening the database.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSummary {
    pub dir_name: String,
    pub meta: Meta,
}
/// Holds exclusive access to a session until dropped.
pub struct SessionLock {
    path: PathBuf,
}
impl SessionLock {
    fn acquire(dir: &Path) -> Result<Self, String> {
        let path = dir.join(LOCK_FILE);
        for attempt in 0..2 {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    let lock = Self { path: path.clone() };
                    let hostname = fs::read_to_string("/proc/sys/kernel/hostname")
                        .unwrap_or_else(|_| "unknown".into());
                    write!(file, "{}\n{}\n", std::process::id(), hostname.trim())
                        .map_err(|e| e.to_string())?;
                    return Ok(lock);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && attempt == 0 => {
                    let text = fs::read_to_string(&path).unwrap_or_default();
                    let mut lines = text.lines();
                    let pid = lines
                        .next()
                        .and_then(|s| s.parse::<u32>().ok())
                        .filter(|p| *p > 0);
                    let pid = pid.filter(|_| {
                        lines.next().is_some_and(|host| !host.trim().is_empty())
                            && lines.next().is_none()
                    });
                    if let Some(pid) = pid {
                        if Path::new(&format!("/proc/{pid}")).exists() {
                            return Err(format!(
                                "session {} is open in process {pid}",
                                dir.file_name().unwrap_or_default().to_string_lossy()
                            ));
                        }
                    }
                    eprintln!("taking over a stale lock from pid {}", pid.unwrap_or(0));
                    fs::remove_file(&path).map_err(|e| e.to_string())?;
                }
                Err(e) => return Err(format!("cannot lock session: {e}")),
            }
        }
        Err("cannot take session lock".into())
    }
}
impl Drop for SessionLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
/// A locked world database and its committed metadata.
pub struct Store {
    dir: PathBuf,
    meta: Meta,
    db: rusqlite::Connection,
    _lock: SessionLock,
}
impl Store {
    /// Lists readable session metadata, newest folder first.
    pub fn list(project: &Project) -> Result<Vec<SessionSummary>, String> {
        let entries = match fs::read_dir(project.rpg_folder()) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.to_string()),
        };
        let mut list = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            if !entry.path().is_dir() {
                continue;
            }
            if let Ok(text) = fs::read_to_string(entry.path().join(META_FILE)) {
                if let Ok(meta) = serde_json::from_str(&text) {
                    list.push(SessionSummary {
                        dir_name: entry.file_name().to_string_lossy().into_owned(),
                        meta,
                    });
                }
            }
        }
        list.sort_by(|a, b| b.dir_name.cmp(&a.dir_name));
        Ok(list)
    }
    /// Creates a dated session with a new world and default roles.
    pub fn create(project: &Project, name: &str) -> Result<Self, String> {
        let name = pascal_case(name);
        if name.is_empty() {
            return Err("a session name needs at least one letter or number".into());
        }
        let created = utc_stamp(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_secs(),
        );
        let dir_name = format!("{}_{}", &created[2..8], name);
        let dir = project.rpg_folder().join(&dir_name);
        fs::create_dir_all(project.rpg_folder()).map_err(|e| e.to_string())?;
        fs::create_dir(&dir).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                format!("session {dir_name} already exists")
            } else {
                e.to_string()
            }
        })?;
        fs::create_dir(dir.join(TRANSCRIPTS)).map_err(|e| e.to_string())?;
        let lock = SessionLock::acquire(&dir)?;
        let db = rusqlite::Connection::open(dir.join(DB_FILE)).map_err(sql_error)?;
        configure(&db)?;
        migrate(&db)?;
        let meta = Meta {
            name,
            created,
            turn: 0,
            game_time: START_TIME.into(),
            current_scene_id: None,
            roles: RoleConfig::default(),
        };
        let mut store = Self {
            dir,
            meta: meta.clone(),
            db,
            _lock: lock,
        };
        store.save_meta(meta)?;
        fs::write(store.dir.join(ECHO_FILE), "").map_err(|e| e.to_string())?;
        Ok(store)
    }
    /// Opens a session, restoring its last backup if integrity checks fail.
    pub fn open(project: &Project, dir_name: &str) -> Result<Self, String> {
        if !matches!(
            Path::new(dir_name)
                .components()
                .collect::<Vec<_>>()
                .as_slice(),
            [Component::Normal(_)]
        ) {
            return Err("invalid session folder name".into());
        }
        let dir = project.rpg_folder().join(dir_name);
        let lock = SessionLock::acquire(&dir)?;
        fn checked(path: &Path) -> Result<rusqlite::Connection, String> {
            let db = rusqlite::Connection::open_with_flags(
                path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
            )
            .map_err(sql_error)?;
            configure(&db)?;
            let check: String = db
                .query_row("PRAGMA quick_check", [], |r| r.get(0))
                .map_err(sql_error)?;
            if check != "ok" {
                return Err(format!("integrity check: {check}"));
            }
            Ok(db)
        }
        let db = match checked(&dir.join(DB_FILE)) {
            Ok(db) => db,
            Err(_) => {
                if !dir.join(BACKUP_FILE).exists() {
                    return Err(format!(
                        "world.db is damaged and there is no backup ({BACKUP_FILE})"
                    ));
                }
                for file in ["world.db-wal", "world.db-shm"] {
                    match fs::remove_file(dir.join(file)) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => return Err(e.to_string()),
                    }
                }
                fs::copy(dir.join(BACKUP_FILE), dir.join(DB_FILE)).map_err(|e| e.to_string())?;
                let db = checked(&dir.join(DB_FILE))
                    .map_err(|e| format!("restored backup failed: {e}"))?;
                eprintln!("world.db failed its integrity check; restored the last backup");
                db
            }
        };
        migrate(&db)?;
        let meta = serde_json::from_str(
            &fs::read_to_string(dir.join(META_FILE)).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("invalid meta.json: {e}"))?;
        Ok(Self {
            dir,
            meta,
            db,
            _lock: lock,
        })
    }
    /// The session's persistent folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
    /// The session's SQLite connection.
    pub fn db(&self) -> &rusqlite::Connection {
        &self.db
    }
    /// The last committed metadata.
    pub fn meta(&self) -> &Meta {
        &self.meta
    }
    /// Writes metadata to a temporary file before a turn commits.
    pub fn stage_meta(&self, meta: &Meta) -> Result<(), String> {
        fs::write(
            self.dir.join("meta.json.tmp"),
            serde_json::to_string_pretty(meta).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }
    /// Publishes staged metadata after a successful commit.
    pub fn finish_meta(&mut self, meta: Meta) -> Result<(), String> {
        fs::rename(self.dir.join("meta.json.tmp"), self.dir.join(META_FILE))
            .map_err(|e| e.to_string())?;
        self.meta = meta;
        Ok(())
    }
    /// Stages and publishes metadata outside a turn.
    pub fn save_meta(&mut self, meta: Meta) -> Result<(), String> {
        self.stage_meta(&meta)?;
        self.finish_meta(meta)
    }
    /// Makes a compact, consistent SQLite backup.
    pub fn checkpoint(&self) -> Result<(), String> {
        let tmp = self.dir.join("world.vacuum.tmp");
        match fs::remove_file(&tmp) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
        self.db
            .execute("VACUUM INTO ?1", [tmp.to_string_lossy().as_ref()])
            .map_err(sql_error)?;
        fs::rename(tmp, self.dir.join(BACKUP_FILE)).map_err(|e| e.to_string())
    }
}
/// Capitalizes alphanumeric words and joins them into a folder-safe name.
pub fn pascal_case(name: &str) -> String {
    name.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            chars
                .next()
                .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    fn project(name: &str) -> Project {
        let root = std::env::temp_dir().join(format!("forgeflow-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        Project::initialize(&root).unwrap()
    }
    #[test]
    fn pascal_case_keeps_letters_and_digits() {
        assert_eq!(pascal_case("crown of ashes!"), "CrownOfAshes");
        assert_eq!(pascal_case("tavern-one shot 2"), "TavernOneShot2");
        assert_eq!(pascal_case("!!"), "");
    }
    #[test]
    fn create_close_reopen_keeps_state() {
        let p = project("rpg-reopen");
        let mut s = Store::create(&p, "Crown").unwrap();
        let dir = s.dir().file_name().unwrap().to_str().unwrap().to_owned();
        s.db()
            .execute(
                "INSERT INTO entities(name,type) VALUES ('Gareth','character')",
                [],
            )
            .unwrap();
        let mut m = s.meta().clone();
        m.turn = 3;
        s.save_meta(m).unwrap();
        drop(s);
        let s = Store::open(&p, &dir).unwrap();
        assert_eq!(s.meta().turn, 3);
        assert_eq!(
            s.db()
                .query_row("SELECT name FROM entities", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "Gareth"
        );
        drop(s);
        std::fs::remove_dir_all(p.root).unwrap();
    }
    #[test]
    fn list_reads_meta_only() {
        let p = project("rpg-list");
        let s = Store::create(&p, "Alpha").unwrap();
        let dir = s.dir().to_owned();
        drop(s);
        drop(Store::create(&p, "Zulu").unwrap());
        std::fs::remove_file(dir.join(DB_FILE)).unwrap();
        let list = Store::list(&p).unwrap();
        assert_eq!(list.len(), 2);
        assert!(list[0].dir_name > list[1].dir_name);
        std::fs::remove_dir_all(p.root).unwrap();
    }
    #[test]
    fn a_held_lock_is_refused() {
        let p = project("rpg-held");
        let s = Store::create(&p, "Held").unwrap();
        assert!(
            Store::open(&p, s.dir().file_name().unwrap().to_str().unwrap())
                .err()
                .unwrap()
                .contains("is open in process")
        );
        drop(s);
        std::fs::remove_dir_all(p.root).unwrap();
    }
    #[test]
    fn a_stale_lock_is_taken_over() {
        let p = project("rpg-stale");
        let s = Store::create(&p, "Stale").unwrap();
        let dir = s.dir().to_owned();
        drop(s);
        std::fs::write(dir.join(LOCK_FILE), "999999999\nhost\n").unwrap();
        drop(Store::open(&p, dir.file_name().unwrap().to_str().unwrap()).unwrap());
        std::fs::remove_dir_all(p.root).unwrap();
    }
    #[test]
    fn opening_a_current_db_runs_no_migrations() {
        let p = project("rpg-current");
        let s = Store::create(&p, "Current").unwrap();
        let dir = s.dir().to_owned();
        drop(s);
        let db = rusqlite::Connection::open(dir.join(DB_FILE)).unwrap();
        assert_eq!(migrate(&db).unwrap(), 0);
        drop(db);
        std::fs::remove_dir_all(p.root).unwrap();
    }
    #[test]
    fn a_torn_write_reopens_cleanly() {
        let p = project("rpg-torn");
        let s = Store::create(&p, "Torn").unwrap();
        let dir = s.dir().to_owned();
        s.db()
            .execute(
                "INSERT INTO entities(name,type) VALUES ('Gareth','character')",
                [],
            )
            .unwrap();
        drop(s);
        std::fs::write(dir.join("world.db-wal"), vec![0xAB; 5000]).unwrap();
        let s = Store::open(&p, dir.file_name().unwrap().to_str().unwrap()).unwrap();
        assert_eq!(
            s.db()
                .query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        assert_eq!(
            s.db()
                .query_row("SELECT count(*) FROM entities", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert!(!dir.join(BACKUP_FILE).exists());
        drop(s);
        std::fs::remove_dir_all(p.root).unwrap();
    }
    #[test]
    fn checkpoint_backup_restores_the_same_data() {
        let p = project("rpg-backup");
        let s = Store::create(&p, "Backup").unwrap();
        let dir = s.dir().to_owned();
        s.db().execute_batch("INSERT INTO entities(name,type) VALUES ('Gareth','character'); INSERT INTO aspects(text,scope,valid_from) VALUES ('On Fire','campaign','1471-03-12T20:00'); INSERT INTO events(turn,game_time,summary) VALUES(1,'1471-03-12T20:00','Fire');").unwrap();
        fn dump(db: &rusqlite::Connection) -> Vec<String> {
            let mut all = Vec::new();
            for table in ["entities", "aspects", "events"] {
                let mut stmt = db
                    .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
                    .unwrap();
                let n = stmt.column_count();
                all.extend(
                    stmt.query_map([], |r| {
                        Ok((0..n)
                            .map(|i| format!("{:?}", r.get_ref(i).unwrap()))
                            .collect::<Vec<_>>()
                            .join("|"))
                    })
                    .unwrap()
                    .map(Result::unwrap),
                );
            }
            all
        }
        let expected = dump(s.db());
        s.checkpoint().unwrap();
        drop(s);
        std::fs::write(dir.join(DB_FILE), b"garbage").unwrap();
        for f in ["world.db-wal", "world.db-shm"] {
            let _ = std::fs::remove_file(dir.join(f));
        }
        let s = Store::open(&p, dir.file_name().unwrap().to_str().unwrap()).unwrap();
        assert_eq!(dump(s.db()), expected);
        drop(s);
        std::fs::remove_dir_all(p.root).unwrap();
    }
    #[test]
    fn project_initialize_ignores_rpg_local_files() {
        let p = project("rpg-ignore");
        let text = std::fs::read_to_string(p.rpg_folder().join(".gitignore")).unwrap();
        for e in ["*/transcripts/", "*/.lock", "*/meta.json.tmp"] {
            assert!(text.lines().any(|l| l == e));
        }
        std::fs::remove_dir_all(p.root).unwrap();
    }
    #[test]
    fn a_malformed_lock_is_taken_over() {
        let p = project("rpg-malformed");
        let s = Store::create(&p, "Malformed").unwrap();
        let dir = s.dir().to_owned();
        drop(s);
        std::fs::write(dir.join(LOCK_FILE), format!("{}\n", std::process::id())).unwrap();
        drop(Store::open(&p, dir.file_name().unwrap().to_str().unwrap()).unwrap());
        std::fs::remove_dir_all(p.root).unwrap();
    }
}

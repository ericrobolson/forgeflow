CREATE TABLE events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    turn INTEGER NOT NULL,
    game_time TEXT NOT NULL,
    real_time TEXT NOT NULL DEFAULT (datetime('now')),
    actor_entity_id INTEGER,
    summary TEXT NOT NULL
);
CREATE TABLE entities (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    type TEXT NOT NULL,
    attributes TEXT,
    created_event_id INTEGER,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE aspects (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    text TEXT NOT NULL,
    scope TEXT NOT NULL,
    holder_id INTEGER,
    slot TEXT,
    valid_from TEXT NOT NULL,
    valid_to TEXT,
    begun_event_id INTEGER,
    ended_event_id INTEGER,
    superseded_by INTEGER,
    chain_root_id INTEGER,
    recorded_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE skills (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_id INTEGER NOT NULL,
    name TEXT NOT NULL,
    rank INTEGER NOT NULL,
    kind TEXT NOT NULL,
    valid_from TEXT NOT NULL,
    valid_to TEXT,
    superseded_by INTEGER,
    chain_root_id INTEGER,
    recorded_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE behaviors (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_id INTEGER NOT NULL,
    kind TEXT NOT NULL,
    trigger TEXT NOT NULL,
    action TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 0,
    valid_from TEXT NOT NULL,
    valid_to TEXT,
    superseded_by INTEGER,
    chain_root_id INTEGER,
    recorded_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE stress_tracks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_id INTEGER NOT NULL,
    name TEXT NOT NULL,
    size INTEGER NOT NULL,
    marked INTEGER NOT NULL DEFAULT 0,
    valid_from TEXT NOT NULL,
    valid_to TEXT,
    superseded_by INTEGER,
    chain_root_id INTEGER,
    recorded_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE relationships (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    from_entity_id INTEGER NOT NULL,
    to_entity_id INTEGER NOT NULL,
    type TEXT NOT NULL,
    score INTEGER NOT NULL DEFAULT 0,
    valid_from TEXT NOT NULL,
    valid_to TEXT,
    superseded_by INTEGER,
    chain_root_id INTEGER,
    recorded_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE scenes (
    id INTEGER PRIMARY KEY,
    title TEXT NOT NULL,
    location_entity_id INTEGER,
    chapter INTEGER NOT NULL DEFAULT 1,
    summary TEXT,
    started_event_id INTEGER,
    ended_event_id INTEGER,
    valid_from TEXT NOT NULL,
    valid_to TEXT,
    superseded_by INTEGER,
    chain_root_id INTEGER,
    recorded_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE presence (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    scene_id INTEGER NOT NULL,
    entity_id INTEGER NOT NULL,
    role TEXT NOT NULL,
    valid_from TEXT NOT NULL,
    valid_to TEXT,
    joined_event_id INTEGER,
    left_event_id INTEGER,
    superseded_by INTEGER,
    chain_root_id INTEGER,
    recorded_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE fate_transactions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_id INTEGER NOT NULL,
    delta INTEGER NOT NULL,
    reason TEXT,
    aspect_id INTEGER,
    event_id INTEGER,
    game_time TEXT NOT NULL,
    recorded_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE free_invokes (
    aspect_id INTEGER NOT NULL,
    beneficiary_entity_id INTEGER,
    count INTEGER NOT NULL DEFAULT 1
);
CREATE VIRTUAL TABLE aspects_fts USING fts5(text, content='aspects', content_rowid='id');
CREATE TRIGGER aspects_fts_insert AFTER INSERT ON aspects BEGIN
    INSERT INTO aspects_fts(rowid, text) VALUES (new.id, new.text);
END;
CREATE INDEX aspects_holder ON aspects(holder_id, valid_to);
CREATE INDEX aspects_chain ON aspects(chain_root_id);
CREATE INDEX presence_scene ON presence(scene_id, valid_to);
CREATE INDEX relationships_from ON relationships(from_entity_id, valid_to);

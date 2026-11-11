# 02 · Schema

Modules: `src/rpg/schema.rs`, `src/rpg/scenes.rs`

## The Bronze Rule as schema

Fate's Bronze Rule — "everything can be treated as a character" — becomes a data-model rule: **every person, place, or thing is an `entities` row, and the five Fate building blocks are first-class tables keyed by `entity_id`.** `type` is open vocabulary (`character`, `location`, `item`, `faction`, `creature`, `scene`, `storm`, `kingdom`, `curse` …); no schema change is needed for a new kind of thing.

| Fate building block | Table | Entity-keyed | Temporal |
|---|---|---|---|
| Aspects (incl. high concept, trouble, situation, consequences) | `aspects` | ✅ `holder_id` = any entity | ✅ `valid_from`/`valid_to` |
| Skills / Approaches | `skills` | ✅ `entity_id` | ✅ versioned |
| Stunts / Behaviors | `behaviors` | ✅ `entity_id` | ✅ `valid_from`/`valid_to` |
| Stress tracks | `stress_tracks` | ✅ `entity_id` | definition versioned; marks = scene state |
| Relationships | `relationships` | ✅ `from`/`to` any entity | ✅ `valid_from`/`valid_to` |
| Fate economy | `fate_transactions` | character-scoped (correct per Fate) | event log |

Everything *can* be a character; not everything *must* have fate points — so the economy stays character-scoped.

## Tables

```sql
-- Spine: every turn/event. Everything else references it.
CREATE TABLE events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    turn        INTEGER NOT NULL,
    game_time   TEXT    NOT NULL,            -- ISO8601 in-world clock
    real_time   TEXT    NOT NULL DEFAULT (datetime('now')),
    actor_entity_id INTEGER,
    summary     TEXT    NOT NULL
);

-- Nodes: anything in the world. type is open vocabulary.
CREATE TABLE entities (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    name          TEXT    NOT NULL,
    type          TEXT    NOT NULL,
    attributes    TEXT,                      -- JSON: hp, coords, freeform props not in a building-block table
    created_event_id INTEGER,
    created_at    TEXT    NOT NULL DEFAULT (datetime('now'))
);

-- Aspects: short, always-true phrases. The workhorse.
CREATE TABLE aspects (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    text            TEXT    NOT NULL,
    scope           TEXT    NOT NULL,        -- character / situation / campaign / item / consequence
    holder_id       INTEGER,                 -- entity id; NULL only for scope='campaign'
    slot            TEXT,                    -- high-concept / trouble / boost / consequence-mild…
    valid_from      TEXT    NOT NULL,        -- game time this became true
    valid_to        TEXT,                    -- game time it stopped being true; NULL = still true
    begun_event_id  INTEGER,
    ended_event_id  INTEGER,
    superseded_by   INTEGER,                 -- successor; NULL = current/terminal
    chain_root_id   INTEGER,                 -- origin of this rename lineage
    recorded_at     TEXT    NOT NULL DEFAULT (datetime('now'))
);

-- Skills and Approaches (kind distinguishes). Rated on the Fate ladder (+0..+8).
CREATE TABLE skills (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_id     INTEGER NOT NULL,
    name          TEXT    NOT NULL,
    rank          INTEGER NOT NULL,
    kind          TEXT    NOT NULL,          -- skill / approach
    valid_from    TEXT    NOT NULL,
    valid_to      TEXT,
    superseded_by INTEGER, chain_root_id INTEGER,
    recorded_at   TEXT    NOT NULL DEFAULT (datetime('now'))
);

-- Stunts and behavioral dispositions: forward-looking rules.
CREATE TABLE behaviors (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_id     INTEGER NOT NULL,
    kind          TEXT    NOT NULL,          -- stunt / disposition
    trigger       TEXT    NOT NULL,          -- when_low_health / on_greeting / when_betrayed
    action        TEXT    NOT NULL,          -- flee / negotiate / +2 to Fight when defending
    priority      INTEGER DEFAULT 0,
    valid_from    TEXT    NOT NULL, valid_to TEXT,
    superseded_by INTEGER, chain_root_id INTEGER,
    recorded_at   TEXT    NOT NULL DEFAULT (datetime('now'))
);

-- Stress tracks: a ship's Hull, an army's Morale, a kingdom's Unrest.
CREATE TABLE stress_tracks (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_id     INTEGER NOT NULL,
    name          TEXT    NOT NULL,
    size          INTEGER NOT NULL,          -- box count
    marked        INTEGER NOT NULL DEFAULT 0,-- current marks (mutable scene state; cleared on recovery)
    valid_from    TEXT, valid_to TEXT, superseded_by INTEGER, chain_root_id INTEGER,
    recorded_at   TEXT    NOT NULL DEFAULT (datetime('now'))
);

-- Temporal edges between any two entities.
CREATE TABLE relationships (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    from_entity_id INTEGER NOT NULL,
    to_entity_id   INTEGER NOT NULL,
    type          TEXT    NOT NULL,          -- ally / enemy / member_of / parent_of / owes / occurs_at…
    score         INTEGER DEFAULT 0,         -- e.g. trust -100..100
    valid_from    TEXT    NOT NULL, valid_to TEXT,
    superseded_by INTEGER, chain_root_id INTEGER,
    recorded_at   TEXT    NOT NULL DEFAULT (datetime('now'))
);
```

### Scenes and presence

A scene is an `entities` row with `type='scene'`. Scene-specific fields live in a 1:1 table; situation aspects hang off the scene entity via `aspects.holder_id`.

```sql
CREATE TABLE scenes (
    id                  INTEGER PRIMARY KEY,        -- == entities.id
    title               TEXT    NOT NULL,
    location_entity_id  INTEGER,
    chapter             INTEGER DEFAULT 1,
    summary             TEXT,                       -- one-line, for context + image prompt
    started_event_id    INTEGER,
    ended_event_id      INTEGER,
    valid_from          TEXT    NOT NULL,           -- game-time window
    valid_to            TEXT,                       -- NULL = active
    superseded_by INTEGER, chain_root_id INTEGER,
    recorded_at         TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE presence (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    scene_id        INTEGER NOT NULL,
    entity_id       INTEGER NOT NULL,
    role            TEXT,                           -- pc / npc / prop / observer
    valid_from      TEXT    NOT NULL,               -- when they joined
    valid_to        TEXT,                           -- when they left; NULL = still here
    joined_event_id INTEGER, left_event_id INTEGER,
    superseded_by INTEGER, chain_root_id INTEGER,
    recorded_at     TEXT    NOT NULL DEFAULT (datetime('now'))
);
```

### Fate economy

Events, not state — a point-in-time log with no `valid_to`.

```sql
CREATE TABLE fate_transactions (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_id   INTEGER NOT NULL,          -- whose points (a character)
    delta       INTEGER NOT NULL,          -- -1 invoke / +1 compel accepted / +N refresh
    reason      TEXT,                      -- "invoke:Rain-slicked Rooftops" / "compel" / "refresh"
    aspect_id   INTEGER,
    event_id    INTEGER,
    game_time   TEXT    NOT NULL,
    recorded_at TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE free_invokes (
    aspect_id            INTEGER NOT NULL,
    beneficiary_entity_id INTEGER,         -- NULL = anyone
    count                INTEGER DEFAULT 1
);

-- Optional keyword search over aspects (built-in FTS5). Poor-man's semantic recall before vectors.
CREATE VIRTUAL TABLE aspects_fts USING fts5(aspect_id UNINDEXED, text, content='aspects');
```

## Temporal validity rules

1. **Append-only.** Never `UPDATE` an aspect/skill/relationship's meaning. To change, expire or rename: close the old row (`valid_to = new.valid_from`, `superseded_by = new.id`) and insert the new one in one transaction.
2. **Contiguous windows.** The `valid_to` of the old equals the `valid_from` of the new — no gap, no overlap. This makes point-in-time queries unambiguous (exactly one version is true at any instant). Enforced in `world.rs`, never by hand.
3. **`chain_root_id`** ties a rename lineage together so "the whole fire saga" is one filtered scan, not a recursive walk.
4. **Game-time, not wall-clock.** `valid_from`/`valid_to` are the in-world clock (flashbacks, "what did they know on day 5"). `recorded_at` is real-time, for audit only.
5. **Events have no `valid_to`.** `events`, `fate_transactions` are points, not spans.

## The three query shapes

```sql
-- 1. Current truth about a holder
SELECT * FROM aspects WHERE holder_id=:id AND valid_to IS NULL AND superseded_by IS NULL;

-- 2. The world AS OF a past game-time (flashback)
SELECT * FROM aspects WHERE holder_id=:id
  AND valid_from <= :as_of AND (valid_to IS NULL OR valid_to > :as_of);

-- 3. Full life of one concept across renames
SELECT * FROM aspects WHERE chain_root_id=:root ORDER BY valid_from;
```

Shape 2 is the payoff: reconstruct the entire world at any past moment, so "the villain reveals what the party didn't know in chapter 1" just works.

## Scope vocabulary (aligned to Fate)

| `scope` | `holder_id` | Fate term | Example |
|---|---|---|---|
| `character` | character entity | character aspect | "I Owe My Brother Everything" |
| `situation` | scene (or location) entity | situation aspect | "Rain-slicked Rooftops" |
| `campaign` | NULL | game/campaign aspect | "The Empire Is Falling" |
| `item` | item entity | item aspect | "Cursed Blade" |
| `consequence` | character entity (+ `slot`) | consequence | "Broken Arm" |

A consequence is just an aspect with `scope='consequence'` and a `slot` — it is simultaneously a fact and a gameplay hook you can invoke or compel.

## Tests

- Every table round-trips through its typed struct (serde ↔ row).
- `add_aspect` then `rename_aspect` produces two rows, contiguous windows, shared `chain_root_id`.
- Point-in-time query returns exactly the version true at `:as_of`, for gap and overlap cases.
- A non-character entity (type='storm') can have aspects, a skill, a stress track, and a relationship — no special-case code paths.
- Scope validation rejects a `situation` aspect with a NULL `holder_id`, and a `campaign` aspect with a non-null one.
- `expire_scene` closes a scene's situation aspects and presence per the carry-over decisions (see [03](03-world-words.md)).

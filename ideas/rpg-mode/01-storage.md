# 01 · Storage

Module: `src/rpg/store.rs`

## Session folder

Sessions live in the project:

```
<repo>/_forgeflow/rpg/261111_CrownOfAshes/
  world.db          # SQLite (WAL) — the single source of truth
  meta.json         # name, created, current chapter/turn/game_time, current_scene_id, role→backend config
  events.jsonl      # optional echo of the event spine, for git-readable diffs (a projection, not the truth)
  transcripts/      # per-turn narrator transcripts                  (gitignored)
  .lock             # one process per session                         (gitignored)
```

### Naming

- Folder: `YYMMDD_<Name>` in local time, matching `ideas/build-mode`'s plan naming minus the time-of-day (one session a day is plenty). `<Name>` is PascalCase, from the name typed at creation.
- Within the DB: integer surrogate keys everywhere; `chain_root_id` for rename lineages; zero-padding not needed because IDs are numeric and queried as integers.

### What goes in git

`world.db`, `meta.json`, and `events.jsonl` are tracked. `transcripts/` and `.lock` are gitignored; `Project::initialize` adds them to `_forgeflow/rpg/.gitignore`. Because `world.db` is binary, the `events.jsonl` echo keeps diffs readable in code review (see below).

## SQLite is the truth

Build mode makes `events.jsonl` the truth and rebuilds projections from it. RPG deviates: **SQLite is the truth.** Reasons:

- The data is relational and temporal — SQLite is a better truth store than jsonl replay.
- The append-only / supersede pattern already makes the DB an event log: every change is an insert, never an update, so history is preserved in the tables themselves.
- Point-in-time queries ("the world as of day 5") are SQL `WHERE valid_from <= … AND (valid_to IS NULL OR valid_to > …)`, not a log replay.

Crash safety: WAL mode (`PRAGMA journal_mode=WAL`) plus `PRAGMA synchronous=NORMAL`. A torn write is recovered by SQLite on next open. A periodic `VACUUM INTO` backup (on session close, and every N turns) guards against corruption.

### The events.jsonl echo (optional projection)

After each committed turn, `Store::echo` appends one line per `events` row and a summary line per aspect/presence change. It is derived from the DB and never read back. If it drifts, it is rebuilt by a `rebuild-echo` subcommand. It exists only so `git diff` shows what happened in a session in plain text.

## Store API

```rust
pub struct Store { dir: PathBuf, _lock: SessionLock, db: rusqlite::Connection }

impl Store {
    pub fn list(project: &Project) -> Result<Vec<SessionSummary>, String>;   // reads meta.json only
    pub fn create(project: &Project, name: &str) -> Result<Store, String>;   // migrate + seed
    pub fn open(project: &Project, dir_name: &str) -> Result<Store, String>; // lock + migrate
    pub fn db(&self) -> &rusqlite::Connection;
    pub fn meta(&self) -> &Meta;
    pub fn save_meta(&mut self, meta: &Meta) -> Result<(), String>;
    pub fn checkpoint(&self) -> Result<(), String>;                          // VACUUM INTO backup
}
```

Migrations are versioned (`PRAGMA user_version`) and forward-only; each is a transaction. `open` runs pending migrations, which is a no-op for a current DB.

## Locks

- **Session lock** (`.lock`): created with `create_new`, holding pid + hostname. If it exists and the pid is not running, it is stale: report and take it over. One `forgeflow rpg` process runs a session at a time. The web UI and the TUI are the same process, so no cross-process lease is needed.
- No tree lock (RPG does not touch the working tree).

## Resume

`Store::open`:

1. Take the session lock.
2. Run pending migrations.
3. Run `PRAGMA integrity_quick`; on failure, restore from the latest `.vacuum` backup and re-apply `events.jsonl` if present (best-effort).
4. Read `meta.json`; if `current_scene_id` points to a scene whose `valid_to` is set (the session was closed mid-cut), offer to resume the previous scene.

Ctrl-C at any point is safe: turns commit atomically (see [04](04-turn-loop.md)), so a crash mid-turn leaves the DB at the last committed turn.

## Tests

- Create, close, reopen: state is identical.
- A torn write (kill mid-insert) reopens cleanly under WAL.
- A stale lock is taken over; a held lock (current pid) is refused.
- Migrations are idempotent: opening a current DB runs no migrations.
- `events.jsonl` echo matches the DB after `rebuild-echo`.
- `VACUUM INTO` backup restores byte-for-byte.

# 08 · Build order

Implementation stages with exit criteria. Each stage lands a compilable, tested slice. Mirrors `ideas/build-mode/11-build-order.md`.

## Stage 0 — Schema and store

`src/rpg/schema.rs`, `src/rpg/store.rs`. Add `rusqlite` (bundled) to `Cargo.toml`.

- Migrations (versioned, forward-only, transactional).
- All tables from [02](02-schema.md): `events`, `entities`, `aspects`, `skills`, `behaviors`, `stress_tracks`, `relationships`, `scenes`, `presence`, `fate_transactions`, `free_invokes`, `aspects_fts`.
- `Store::list`/`create`/`open`/`checkpoint`; session lock; WAL.

**Exit:** create a session, reopen it, run every query shape from [02](02-schema.md); a torn write reopens cleanly; a stale lock is taken over.

## Stage 1 — World words

`src/rpg/world.rs`, `src/rpg/scenes.rs`.

- Every query and mutate word from [03](03-world-words.md), each one transaction.
- `get_scene_context`.
- Temporal validity rules enforced (append-only, contiguous windows, `chain_root_id`).
- Validation: rejects empty/over-long text, scope/holder mismatch, `valid_from` before current game_time.

**Exit:** the [02](02-schema.md) and [03](03-world-words.md) test suites pass; a non-character entity (type=`storm`) can hold aspects, a skill, a stress track, and a relationship with no special-case code.

## Stage 2 — Backends and roles

`src/rpg/backend.rs`, `src/rpg/prompts/*.md`.

- `Generation` and `Decision` traits.
- `RulesBackend` (compel + salience heuristics).
- `LocalLlmBackend` for `Generation` (reuse the existing agent loop with world words as tools) and for `Decision` (grammar-constrained JSON).
- `JevBackend` (stubbed HTTP; real key optional).
- `Roles::load` from `meta.json`.

**Exit:** a scripted turn runs end-to-end with `compel_decider = "rules"` and no second model; the `Decision` test table passes for all three backends.

## Stage 3 — Turn loop

`src/rpg/turn.rs`.

- `run_turn`: load → compel → narrate → archive → commit, atomic.
- Scene lifecycle within turns (begin/join/leave/end/cut).
- Flashback turns (past `game_time`, point-in-time load).

**Exit:** the [04](04-turn-loop.md) test suite passes; a panic in archive leaves the DB at the previous turn.

## Stage 4 — TUI

`src/rpg/mod.rs`, `src/rpg/ui.rs`. `main.rs` arm `["rpg"]`; `messages.rs` `/rpg` slash arm.

- Session pick/create menu, sample-world seed.
- Turn prompt; `/scene`, `/aspects`, `/history`, `/as-of`, `/serve`, `/back`, `/quit`.
- GM debug view (full event log) with `/debug off` toggle.

**Exit:** `forgeflow rpg` creates a session, seeds the sample world, runs three scripted turns, and `/as-of` shows a correct past snapshot.

## Stage 5 — Web UI

`src/web_util.rs` (factor from `src/web.rs`), `src/rpg/web.rs`, `src/rpg/scene.rs`.

- `/rpg serve`: QR + token + cookie (reuse `web_util`).
- RPG page; narrative-only renderer; `/send`→`run_turn`; SSE stream.
- 🖼 / 🎬 Scene buttons: `scene.rs` assembler → `make-image`/`make-video`.

**Exit:** scan the QR on a phone, play three turns with narration only (no commands visible), tap 🖼 and see an inline scene image; `forgeflow serve` still works unchanged after the `web_util` refactor.

## Stage 6 — Polish

- `events.jsonl` echo + `rebuild-echo`.
- `VACUUM INTO` backups on close and every N turns.
- `LocalLlmBackend` grammar file for `DecisionAnswer`.
- Optional vector layer hook (FTS5 remains the default).

**Exit:** a one-hour play session on a phone with a local narrator and `RulesBackend` compels, no crashes, a readable `git diff` of the session.

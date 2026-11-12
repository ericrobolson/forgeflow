> Implemented; see planner/261111-132135_RpgMode.md for the decisions that refine this design.

# RPG mode

`forgeflow rpg` (and `/rpg` in chat) runs a tabletop-RPG world inside ForgeFlow. A named session owns a SQLite database that is the single source of truth for the world: entities, aspects, skills, stress, relationships, behaviors, scenes, and the fate-point economy. A local model narrates; the player just types what they do. `/rpg serve` opens a phone-clean web UI (QR-scanned) that shows only the story, with one-tap scene images and video.

This folder is the design. Each file covers one part and lists its module, types and tests.

## Why

A local LLM can't hold a growing world in context, and stuffing it degrades recall (context rot). Fate's design already solved which facts matter: **aspects are the dramatically-relevant truths**, scoped to what they're on. So the world is a curated, temporal, relational store, and the LLM is handed exactly the slice the current scene needs.

Fate's **Bronze Rule — "everything can be treated as a character"** becomes a schema principle: every person, place, or thing is an entity, and the five Fate building blocks (aspects, skills, stunts, stress, consequences) are first-class tables keyed by `entity_id`. A storm and a bard are indistinguishable to the model.

## Core rules

- **The DB is the truth.** SQLite (WAL) is the single source of truth, not a jsonl replay. Aspects are append-only with `valid_from` / `valid_to` / `superseded_by`; nothing is `UPDATE`d, only superseded.
- **Models write JSON; Rust checks it.** Every model-proposed aspect, skill change, or compel decision is parsed into a typed struct and validated before it touches the DB. A bad answer goes back with errors.
- **Scenes are the context boundary.** The current scene defines what is loaded each turn — situation aspects, present entities, recent narration — in one bounded query. No relevance ranking needed.
- **Roles, not models.** Narrator, archivist, and compel-decider are roles mapped to backends in config; code never names a provider. The compel-decider is a black box (Jev, a local LLM, or rules).
- **The player sees only the story.** The web UI hides programs, tool receipts, and the stack; the narrator's tool calls are the hidden "translation."

## The flow

```
forgeflow rpg  (or /rpg)
  └─ pick session / new session
       └─ each turn:
            load scene context  (aspects, presence, relationships, recent narration)
              └─ compel pass: per active aspect, the decision backend returns compel? yes/no
                   └─ narrate: narrator model (agent loop + world tools) generates the response,
                        calling log-event / add-aspect / update-relationship / join-scene …
                         └─ archive: archivist extracts salient facts the narrator implied, persists
                              └─ persist; new state committed; receipt shown
```

## Decisions

| Area | Decision | Detail |
|---|---|---|
| Entry point | `forgeflow rpg` and `/rpg`; `/rpg serve` for the web UI | [06](06-tui.md), [07](07-web-ui.md) |
| Storage | `_forgeflow/rpg/<YYMMDD>_<Name>/world.db` (SQLite, WAL); one DB per session | [01](01-storage.md) |
| Truth | SQLite is the truth (not jsonl); aspects append-only + supersede | [01](01-storage.md) |
| Schema | entities + five Bronze-Rule building blocks + scenes/presence + fate economy, all temporal | [02](02-schema.md) |
| World ops | typed ForgeFlow words → agent tool schemas; results in scratch registers | [03](03-world-words.md) |
| Turn loop | load scene → decide compels → narrate → archive → persist | [04](04-turn-loop.md) |
| Backends | narrator (generation) + archivist + compel-decider (decision); role→backend in config; the compel-decider is a black box | [05](05-backend-roles.md) |
| TUI | session pick/create menu; GM debug view (full event log) | [06](06-tui.md) |
| Web UI | QR-served, narrative-only renderer, scene image/video button; shares transport with `forgeflow serve` | [07](07-web-ui.md) |

## Module layout

```
src/
  web_util.rs     # shared transport (qr, lan_ip, token, multipart, markdown, file, sse) factored from web.rs
  web.rs          # refactored to use web_util (no behavior change)
  rpg/
    mod.rs        # run() entry: session menu + driver loop
    store.rs      # session folders, SQLite open/migrate/WAL, locks, resume                (01)
    schema.rs     # SQL schema, migrations, typed structs for every table                  (02)
    scenes.rs     # scenes + presence, lifecycle (begin/end/cut/join/leave), get_scene_context  (02)
    world.rs      # function layer: add/rename/expire aspect, update-relationship, stress…  (03)
    tools.rs      # ForgeFlow words → agent tool schemas                                    (03)
    turn.rs       # turn loop: load → decide → narrate → archive → persist                  (04)
    backend.rs    # role→backend, Generation + Decision traits, ask/validate                (05)
    ui.rs         # session pick/create, turn prompt, GM debug view                         (06)
    web.rs        # RPG page, narrative renderer, /send → run_turn                          (07)
    scene.rs      # scene→prompt assembler for the image/video buttons                      (07)
    prompts/*.md  # narrator / archivist / compel prompts, overridable per project
```

`main.rs` gains one arm: `["rpg"] => forgeflow::rpg::run(context)`; `messages.rs` gains a `/rpg` slash arm.

## New dependencies

- `rusqlite` with the `bundled` feature — SQLite with no system dependency.
- (reuse) `qrcode`, `tiny_http`, `maud`, `pulldown-cmark`, `image`, `ureq` — already in `Cargo.toml`.

## Out of scope for v1

- Vector / semantic search — FTS5 (SQLite built-in) for keyword recall first; embeddings later.
- Jev/cloud decision backend wired by default — v1 compel-decider defaults to the narrator or a rules backend; the `Decision` trait and Jev impl exist but are opt-in.
- Dice mechanics engine — v1 narrates outcomes; a deterministic `roll` word comes later.
- Multi-player / networked sessions — one player per session; the session lock serializes.
- A GM-facing aspect editor in the web UI — v1 the narrator mutates the world via tools only.

## Relation to build mode

This mirrors `ideas/build-mode/`: a design folder paired with a `src/rpg/` module, `main.rs` gaining one arm, storage under `_forgeflow/`, "models write JSON; Rust checks it," and role→backend config. The deviation is SQLite-as-truth instead of events.jsonl-as-truth, because the data is relational and temporal (see [01](01-storage.md)).

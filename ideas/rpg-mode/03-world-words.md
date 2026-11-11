# 03 · World words

Module: `src/rpg/world.rs`, `src/rpg/tools.rs`

## The function layer

The LLM never writes SQL. World operations are typed ForgeFlow words, which the existing agent loop auto-exposes to the narrator model as tools (schemas generated from each word's typed inputs, results into scratch registers R4–R15, short receipts back to the model — the same machinery as `src/agent_loop.rs`). This is the clean fit: zero new agent infrastructure.

Following build mode's **"models write JSON; Rust checks it"** rule, every mutating word parses its arguments into a typed struct and validates before insert.

### Query words

| Word | Inputs | Returns | Notes |
|---|---|---|---|
| `scene` | — | scene context bundle | the current scene: title, summary, situation aspects, present entities, location aspects, recent narration |
| `aspects` | `holder_id` | list | current aspects (`valid_to IS NULL AND superseded_by IS NULL`) |
| `aspects-at` | `holder_id`, `game_time` | list | point-in-time view — flashbacks |
| `entity` | `id` | record | name, type, attributes, current skills, stress |
| `relationships` | `from_id`, `to_id?` | list | current edges (+ score) |
| `behaviors` | `entity_id` | list | active stunts/dispositions |
| `skills` | `entity_id` | list | current ratings |
| `stress` | `entity_id` | list | tracks + current marks |
| `search-aspects` | `keywords` | list | FTS5 keyword recall |

### Mutate words

| Word | Inputs | Effect |
|---|---|---|
| `log-event` | `turn`, `game_time`, `summary` | append an `events` row (the spine) |
| `add-aspect` | `text`, `scope`, `holder_id`, `slot?`, `valid_from` | insert; auto-supersedes a conflicting prior aspect of the same scope/slot on the holder |
| `rename-aspect` | `id`, `new_text` | close old (`valid_to`, `superseded_by`), insert new, share `chain_root_id` — one txn |
| `expire-aspect` | `id`, `valid_to`, `ended_event_id?` | set `valid_to`; no successor |
| `update-relationship` | `from`, `to`, `type`, `score`, `valid_from` | close old edge, open new — one txn |
| `set-skill` | `entity_id`, `name`, `rank`, `valid_from` | supersede prior rating |
| `mark-stress` / `clear-stress` | `entity_id`, `track`, `delta` | mutate `marked` (scene state) |
| `join-scene` / `leave-scene` | `entity_id`, `scene_id` | mutate `presence` (temporal) |
| `begin-scene` | `title`, `location_id?`, `chapter`, `participants[]`, `situation_aspects[]` | create scene entity + `scenes` + `presence` + aspects; set `current_scene_id` in `meta.json` |
| `end-scene` | `scene_id`, `carry_over[]` | per listed situation aspect: carry (rescope) or expire; set `valid_to`; close departing presence |
| `cut-to-scene` | `scene_id` | move `current_scene_id` pointer (flashbacks/cutaways) |
| `spend-fate` / `award-fate` | `entity_id`, `delta`, `reason`, `aspect_id?` | append `fate_transactions` row |

### Validation (the "Rust checks it" half)

A model-proposed `add-aspect` is parsed into:

```rust
pub struct AspectInput {
    pub text: String,
    pub scope: Scope,           // enum, validated against the table above
    pub holder_id: Option<i64>, // required unless scope == Campaign
    pub slot: Option<String>,
    pub valid_from: String,     // ISO8601, must be >= the session's current game_time
}
```

Checks before insert: `text` non-empty and ≤ 80 chars (an aspect is a short phrase); `scope`/`holder_id` consistency (see [02](02-schema.md)); `valid_from` not before the world's current game_time; contiguous-window enforcement against the prior version. A failed check returns a typed error to the model, which may retry once.

## Per-session custom words

Each session may define extra world verbs in `_forgeflow/rpg/<session>/world_words.ff`, loaded by the existing project-words system (`src/user_words.rs`). A homebrew rule ("roll a d20 for luck") becomes a word the narrator can call, validated and activated with `/reload` as today.

## Tools → agent schemas

`tools.rs` derives each word's tool schema from its typed inputs (as `src/ops.rs` does for `MakeImage`, etc.), registers them with the agent loop, and maps tool results into scratch registers. The narrator model sees a small JSON receipt ("aspect #142 added"), not the row — keeping small local models' context small, exactly as the existing agent loop does.

## Tests

- Each mutate word is one transaction; a mid-word panic leaves the DB unchanged.
- `rename-aspect` yields contiguous windows and a shared `chain_root_id`.
- `add-aspect` with a conflicting scope/slot supersedes the prior automatically.
- `end-scene` carries over exactly the listed aspects and expires the rest; presence closes for departers only.
- Validation rejects: empty text, over-length text, scope/holder mismatch, `valid_from` before current game_time.
- Tool schemas round-trip: every word produces a valid JSON schema accepted by llama-server.

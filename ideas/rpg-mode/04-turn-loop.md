# 04 · Turn loop

Module: `src/rpg/turn.rs`

## One turn

```
player action (free text)
  │
  1. load scene context      ← get_scene_context(current_scene_id) → formatted prompt slice
  2. compel pass             ← for each active aspect: decision backend → compel? yes/no
  3. narrate                 ← narrator model, agent loop + world tools, streams the response
  4. archive                 ← archivist extracts salient facts the narrator implied, persists
  5. commit                  ← one transaction; meta updated (turn, game_time, current_scene_id)
```

### 1. Load scene context

`scenes::get_scene_context(current_scene_id)` runs the bounded query bundle from [02](02-schema.md): the scene's title/summary + situation aspects + present entities (each with current character aspects, relationships to other present entities, active behaviors, skills, stress) + location aspects + the last N events in this scene. The result is rendered to a compact prompt section. This is the *only* world data the narrator sees — the scene is the relevance boundary, so no classifier is needed to rank facts.

### 2. Compel pass

For each active aspect in scope (scene situation aspects + present characters' aspects), the **decision backend** ([05](05-backend-roles.md)) answers `Compel { aspect_id, scene_id, action_text }` → `{ yes, confidence, reason }`. Aspects flagged `yes` are handed to the narrator as suggested compels ("*Hot-headed* may bite here"). The narrator decides whether to fire them in the narration — the backend only flags; the System-2 model writes the scene. (v1 default backend: `RulesBackend` or the narrator itself; see [05](05-backend-roles.md).)

### 3. Narrate

The narrator model runs in the existing agent loop (`src/agent_loop.rs`) with the world words from [03](03-world-words.md) as tools. Its system prompt holds the loaded scene context and any flagged compels. As it writes, it calls tools to query and mutate the world: `log-event`, `add-aspect`, `update-relationship`, `join-scene`/`leave-scene`, `mark-stress`, `spend-fate`/`award-fate`. Tool results land in scratch registers; the model gets short receipts. The streamed reply (Kind::Chat) is what the UI shows; the tool calls are hidden.

If the narration establishes a scene change ("you travel to the tavern"), the narrator calls `end-scene` + `begin-scene` itself, or the orchestrator detects an unmade `begin-scene` after a location change and prompts one.

### 4. Archive

The **archivist** role reads the turn's narration + tool calls and extracts salient facts the narrator implied but did not explicitly write (a new NPC mentioned, a mood, a promise made). For each candidate it asks the decision backend `Salient { candidate, scene_id }` → keep/drop, then `add-aspect`/`update-relationship` for the kept ones. This separates "writing the scene" (narrator) from "bookkeeping the world" (archivist), so the narrator can focus on prose and the world stays complete.

### 5. Commit

All mutations from steps 3–4 land in one SQLite transaction. `meta.json` is updated: `turn += 1`, `game_time` advances per the narration, `current_scene_id` if it changed. On commit, the `events.jsonl` echo is appended. A panic at any earlier step leaves the DB at the previous turn — the turn is atomic.

## Point-in-time and flashbacks

A flashback is a turn whose `game_time` is in the past. `cut-to-scene` to a past scene (or a freshly created one with a past `valid_from`), run the turn; `aspects-at`/point-in-time queries return the world as it was then. The narrator's prompt notes it is a flashback so it does not "remember" the future.

## Scene lifecycle within turns

| Transition | Trigger | Effect |
|---|---|---|
| begin | narrator calls `begin-scene`, or orchestrator prompts one after a location change | new scene entity + presence + situation aspects; `current_scene_id` set |
| join / leave | narrator calls `join-scene`/`leave-scene` | `presence` mutated (temporal) |
| end | narrator calls `end-scene`, or session close | per-aspect carry-over decision; scene `valid_to` set |
| cut / resume | `cut-to-scene` | `current_scene_id` pointer moved (flashback/cutaway) |

## Driver

```rust
pub fn run_turn(store: &mut Store, roles: &Roles, action: &str) -> Result<TurnReceipt, TurnError> {
    let ctx = scenes::get_scene_context(store.db(), store.meta().current_scene_id)?;
    let compels = roles.compel_decider.compel_pass(&ctx, action)?;
    let narration = roles.narrator.narrate(&ctx, &compels, action, store)?;   // streams; calls world tools
    roles.archivist.archive(store, &narration)?;
    store.commit_turn(&narration.event_ids)?;
    Ok(TurnReceipt { /* … */ })
}
```

`Roles` ([05](05-backend-roles.md)) holds the three backends. The narrator's `narrate` runs the agent loop and streams events to the active sink (TUI or web), exactly as `Conversation::turn` does today.

## Tests

- A scripted narrator + scripted decision backend run a turn end-to-end in memory; the DB ends in the expected state.
- A turn that panics in step 4 (archive) leaves the DB at the previous committed turn.
- A flashback turn (past `game_time`) loads the correct point-in-time aspects.
- `begin-scene` mid-turn updates `current_scene_id` and the next turn loads the new scene.
- Compel flags from the decision backend appear in the narrator's prompt; a `yes` flagged for a trouble-slot aspect is narrated as a compel at least once in a scripted scenario.

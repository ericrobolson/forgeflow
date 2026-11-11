# 06 · TUI

Module: `src/rpg/mod.rs`, `src/rpg/ui.rs`

## Entry points

Two ways in, both calling `rpg::run(context)`:

- **`forgeflow rpg`** — a new arm in `main.rs`: `["rpg"] => forgeflow::rpg::run(context)`.
- **`/rpg`** — a new slash arm in `messages.rs`, alongside `/edit`, `/clear`, `/reload`. It switches the chat session into RPG mode.

## Session pick / create

Mirrors build mode's `ui::pick_plan`. Menus are numbered, one key per choice, built with `crossterm` like `src/setup.rs`.

```
$ forgeflow rpg
RPG sessions in <repo>:
  1  261111_CrownOfAshes      chapter 2 · turn 147 · 3 active PCs
  2  261110_TavernOneShot     turn 12 · 4 active PCs
  n  new session
  q  quit
> n
Name: CrownOfAshes
  created _forgeflow/rpg/261111_CrownOfAshes/world.db
  seed the world? (b)lank, (s)ample
> s
  sample world loaded: 2 PCs, 3 NPCs, 1 location, opening scene begun
```

A new session runs schema migrations and optionally seeds a sample world (a couple of characters with high concept + trouble, a location, and an opening scene) so the first turn has context.

## The turn prompt

After a session is open, the prompt is a plain free-text input — the player's action:

```
[CrownOfAshes · ch 2 · turn 147 · scene: The Bridge at Midnight]
> I draw my blade and tell Gareth to step aside.
```

Ordinary input is a player action → `turn::run_turn`. No `;` translation, no stack. The narrator's reply streams as Markdown, as chat does today. Slash commands available in the prompt:

| Command | Effect |
|---|---|
| `/scene` | print the current scene: title, situation aspects, present entities, location |
| `/aspects <entity>` | list an entity's current aspects |
| `/history <aspect>` | the rename lineage of one aspect |
| `/as-of <game_time>` | the world at a past moment (read-only) |
| `/serve` | start the web UI for this session (see [07](07-web-ui.md)) |
| `/back` | session menu (keeps the session open) |
| `/quit` | close the session (checkpoint + release lock) |

## GM debug view

The TUI is the GM's view, so it shows the **full** event log, unlike the web UI. After each turn it prints:

```
turn 148 · game_time 1471-03-12T22:14
  event #412  party drew blades at the bridge
  aspect #143  "Cornered"  added (situation, scene #7)
  relationship  Gareth→party  enemy  score -40  (was -10)
  compel  "Hot-headed" → yes (rules, 0.8)  — fired
  fate  party  -1  invoke "Cornered"
```

This is the mirror of what the web UI hides. A `/debug off` toggle suppresses it for a cleaner play view.

## Driver

```rust
pub fn run(mut context: Context) -> Result<(), Box<dyn Error>> {
    let session = ui::pick_session(&context.project)?;            // or create
    let mut store = Store::open_or_create(&context.project, session)?;
    let roles = Roles::load(store.meta(), &context.project)?;
    loop {
        let action = ui::read_action(&store)?;
        match action {
            Action::Turn(text) => turn::run_turn(&mut store, &roles, &text)?,
            Action::Command(cmd) => ui::run_command(&mut store, &context, cmd)?,
            Action::Back => break,
        }
    }
    store.checkpoint()?;
    Ok(())
}
```

Ctrl-C at any point is safe: the current turn commits atomically or not at all ([04](04-turn-loop.md)).

## Tests

- `pick_session` lists sessions from `meta.json` only, without opening DBs.
- A new session with the sample seed has an opening scene with `current_scene_id` set and ≥1 PC present.
- `/as-of` returns a read-only snapshot and writes nothing.
- `/serve` starts the web UI bound to the open session's `world.db`.

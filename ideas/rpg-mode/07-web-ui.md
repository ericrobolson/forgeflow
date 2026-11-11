# 07 · Web UI

Module: `src/rpg/web.rs`, `src/rpg/scene.rs`, `src/web_util.rs`

A phone-clean narrative surface served by QR scan. It reuses `forgeflow serve`'s transport (QR, token auth, SSE streaming, multipart, file serving, Markdown) and swaps only the page, the renderer, and the turn runner.

## Entry

`/rpg serve [port]` (slash, from within an open RPG session) or `forgeflow rpg serve [port]` — starts a `tiny_http` server bound to *that session's* `world.db`, prints the QR + `http://<lan-ip>:<port>/?t=<token>` exactly as `WebControl::start` does today. Scan with a phone → cookie auth → narrative UI. Same `qr_code()`, `lan_ip()`, token-in-query-then-cookie flow.

## Shared transport

Factor the existing `src/web.rs` helpers into `src/web_util.rs` (no behavior change to `forgeflow serve`):

`qr_code`, `lan_ip`, `authorized` + cookie flow, `parse_multipart`, `markdown` (script/remote-image safe), `file` serving with byte ranges, `sse_frame`, `save_upload`.

`src/rpg/web.rs` reuses all of it. Only the page, the `/send` handler, and the event renderer are RPG-specific.

## The page

Minimal — no mode radios, no `;`/`/` hints, no code/image/video tabs:

```
┌─ CrownOfAshes · ch 2 · turn 147 ────────────┐
│  (narration scroll — Markdown)                │
│  …the bridge groans under the rain. Gareth    │
│  turns, jaw set. "I told you not to come."    │
│  [inline scene image, if generated]           │
├──────────────────────────────────────────────┤
│  [ 🖼 Scene ]  [ 🎬 Scene ]                    │
│  ┌────────────────────────────────────────┐   │
│  │ What do you do?                    ↑   │   │
│  └────────────────────────────────────────┘   │
└──────────────────────────────────────────────┘
```

- **Composer**: a textarea + send. Two optional buttons: **🖼 Scene** and **🎬 Scene**.
- **Header**: session name, chapter, turn — from `meta.json`.

## Narrative-only renderer (the key delta)

Today `render_events` shows everything: the `→ program` line, run/progress/stack/note lines, receipts. The RPG UI uses `render_narration`, which filters to:

| Event | Shown? |
|---|---|
| `Event::Text(Kind::Chat, …)` | ✅ as Markdown (the narration) |
| `Event::Image(path)` / `Event::Video(path)` | ✅ inline figure |
| `→ program`, run, note, progress, stack, receipts | ❌ suppressed |

The player sees a clean story scroll. The TUI mirror ([06](06-tui.md)) keeps the full debug log.

## `/send` → `run_turn` (no `turn_program`)

The chat web UI builds a `turn_program` string and hands it to `Conversation::turn`. The RPG UI never does. The player's free text is a **player action**, posted to `/send`, which calls `turn::run_turn(store, roles, action)` — the RPG turn loop, not `Conversation::turn`. The narrator's tool calls *are* the hidden "translation"; the UI sees only narration.

- **Terminal chat**: `; text` → translator → program → run *(commands visible)*
- **RPG web**: free text → narrator agent loop with world tools *(commands hidden, narration shown)*

## The 🖼 / 🎬 Scene button

The button does not ask the player for a prompt. It **auto-derives one from the current scene** via `scene.rs`:

1. Query the session DB: scene `title` + `summary`, active situation aspects, present entities + their salient character aspects, location aspects.
2. Assemble a scene prompt: *"rain-slicked rooftop at night, a small fire burning, Gareth angry, the party cornered"* — built from `Rain-slicked Rooftops` + `On Fire` + Gareth's active state.
3. Call the existing **`make-image`** word (or `make-video`/`draft-video`) with that prompt → save to `.tmp/` → render inline via `Event::Image`/`Event::Video`.

No new generation infra — reuses `image_gen`/`video_gen` and the `make-image`/`make-video` words. The only new code is `scene.rs`'s prompt assembler, which is a query against the aspect tables from [02](02-schema.md). For video, the most recent scene image seeds as a keyframe through the existing `draft-video` path.

The scene-prompt tone (moody/bright/tense) can optionally come from a `Tone` decision query ([05](05-backend-roles.md)).

## Routes

```
GET  /                       RPG page (token-gated)
POST /send                   player action → run_turn; reply streams
GET  /stream/<id>            SSE stream of the turn's narration
GET  /files/<path>           images/videos (byte ranges, as today)
GET  /scene/image            run the 🖳 assembler + make-image, push an Image event
GET  /scene/video            run the 🎬 assembler + make-video, push a Video event
GET  /assets/*               htmx, sse.js, page.js, style.css
```

## Why this is a small delta

| Need | Already exists |
|---|---|
| QR + phone serve | `WebControl::start`, `qr_code`, `lan_ip` ✅ |
| Token auth + cookie | `authorized`, `COOKIE` ✅ |
| Streaming replies | SSE in `stream()` ✅ |
| Markdown rendering | `markdown()` ✅ |
| Serving images/videos | `file()` with ranges ✅ |
| Generate scene image | `make-image` + `image_gen` ✅ |
| Generate scene video | `make-video`/`draft-video` + `video_gen` ✅ |

Genuinely new: the narrative-only event filter, `scene.rs` prompt assembler, and `/send`→`run_turn` wiring. Everything else is reuse.

## Tests

- `render_narration` includes `Kind::Chat` text and `Image`/`Video` figures, and excludes `Kind::Note`/`Run`/`Progress` lines — snapshot test.
- `/send` with a player action calls `run_turn`; the response markup is a streaming turn, not a `turn_program`.
- `scene.rs` assembler produces a deterministic prompt for a given scene context (snapshot).
- 🖼 button: a stub `make-image` returns a path; an `Image` event is pushed and the SSE stream carries it.
- Token/cookie flow is identical to `forgeflow serve` (reuses `web_util`).

# Build mode

`forgeflow build` turns an idea into working code. People set the direction, a strong model plans in detail, people approve the plan, cheap models write the code in many small tasks, and deterministic checks decide what counts as done. It works at every size, from "change this web page" to a whole app such as a GBA decompiler.

This folder is the design. Each file below covers one part of the implementation and lists its module, types and tests.

## Why

Cheap models write good code when four things are true:

1. **The task is small.** One task fits in one context window, writes at most a couple of files, and has its scope stated explicitly.
2. **Pass or fail is programmatic.** Every acceptance criterion is a command that runs. If a check needs an external service, a simulator gets built first.
3. **Mistakes are compile errors.** Invariants are encoded in types first, and baseline checks (build, lint, tests) run on every task.
4. **The harness does the bookkeeping.** Ordering, file conflicts, retries, reviews and status tracking are code, not model judgment.

Big models are expensive, so they do the work where judgment pays: detailed planning, auditing, splitting work into tasks, security review, and rescuing tasks that failed. Everything else is Rust.

## Core rules

- **Models write JSON; Rust checks it.** Every model answer is parsed into a typed struct and checked before it changes anything. A bad answer is sent back with its errors.
- **The event log is the truth.** Every change is an event in `events.jsonl`. Everything else in the plan folder is rebuilt from it, so any crash can be resumed.
- **`done` is checked, not reported.** A task completes only when the harness has run the scope check, the baseline and the task's own checks itself.
- **Outside the tree lock, the working tree equals `HEAD`.** Executors never see each other's half-finished edits (see [07-execution](07-execution.md)).
- **Generated files are deterministic.** The same events always produce byte-identical JSON and Markdown. Timestamps appear only in events.

## The flow

```
forgeflow build
  └─ pick plan / new plan
       └─ resolve open issues first  (if any)
            └─ next action for the plan's state:

  INTAKE ──► DIRECTION ──► ┌──────────── per milestone ─────────────────────────────────┐
  human +     model drafts,│ DETAIL ─► AUDIT ⟲ ─► APPROVAL ─► SPLIT ⟲ ─► SCHEDULE ─►     │
  model       human edits  │ model     model+lint  human       model+lint  checks only    │
  suggests                 │                                                             │
  scale                    │ EXECUTE (waves) ─► wave CHECKPOINT ─► … ─► MILESTONE REVIEW  │
                           │ cheap model         human                   human           │
                           │   ↳ fail ×2 → big model ×1 → issue          ↳ REPLAN (apps)  │
                           └─────────────────────────────────────────────────────────────┘
```

## Decisions

| Area | Decision | Detail |
|---|---|---|
| Entry point | `forgeflow build`, a menu over a state machine with one next action per state; no plan subcommands | [02](02-state-machine.md) |
| Storage | `<repo>/_forgeflow/plans/<YYMMDD-HHMMSS>_<Name>/`, numbered JSON + Markdown per phase, a folder per milestone | [01](01-storage.md) |
| Scale | tiny / feature / app; the model suggests, the human confirms. Apps get an M00 scaffold and rolling milestone planning | [03](03-scale-and-milestones.md) |
| Ownership | Human: direction and architecture. Big model: detail, audit, split. Human: approve. Cheap model: execute. Human: wave checkpoints | [02](02-state-machine.md) |
| Executor | Chosen per plan at intake: `loop` (ForgeFlow's own loop) or `cli:<agent>[:<model>]` | [07](07-execution.md), [08](08-loop-write-tools.md) |
| Task rules | runnable acceptance checks, simulators, types-first invariants, red → green → refactor, diff budget, baseline checks | [05](05-audit-and-lint.md), [06](06-split-and-schedule.md) |
| Isolation | File locks in one working tree; a tree lock serializes everything that touches it | [06](06-split-and-schedule.md), [07](07-execution.md) |
| Failure | cheap ×2 → big ×1 → issue | [07](07-execution.md) |
| Supervision | Pause at the end of every wave; security issues always pause | [09](09-gates-and-issues.md) |
| Issues | Resolved first whenever a plan is resumed; `learn` turns a recurring pattern into a lint check | [09](09-gates-and-issues.md) |
| Providers | Roles map to backends in config; plans never name a model directly | [10](10-backends-and-prompts.md) |

## Files

| File | Covers |
|---|---|
| [01-storage.md](01-storage.md) | Plan folder layout, events, rebuilding files from events, locks, resume, determinism |
| [02-state-machine.md](02-state-machine.md) | Phases, who owns each step, transitions, the `forgeflow build` terminal UI |
| [03-scale-and-milestones.md](03-scale-and-milestones.md) | Intake, scale, direction, M00 scaffold, milestone review, replanning |
| [04-phase-schemas.md](04-phase-schemas.md) | Rust types for every phase file |
| [05-audit-and-lint.md](05-audit-and-lint.md) | Deterministic lint, the model audit loop, `build.json` checks, `learn` |
| [06-split-and-schedule.md](06-split-and-schedule.md) | Task schema, splitting, size limits, conflict graph, waves, ready set |
| [07-execution.md](07-execution.md) | Briefs, executors, tree lock, verified done, escalation, reports |
| [08-loop-write-tools.md](08-loop-write-tools.md) | Write tools for ForgeFlow's loop that buffer changes per task and are limited to the task's write set |
| [09-gates-and-issues.md](09-gates-and-issues.md) | Gates, reviews, findings → issues, triage, checkpoints |
| [10-backends-and-prompts.md](10-backends-and-prompts.md) | Role → backend config, `Agent` changes, structured answers, prompt files |
| [11-build-order.md](11-build-order.md) | Implementation stages with exit criteria |

## Module layout

```
src/build/
  mod.rs          # `run()` entry from main.rs: menu + driver loop
  store.rs        # plan folders, events, locks, atomic writes           (01)
  state.rs        # PlanState, reducer, next_action()                    (02)
  ui.rs           # menus, editor round-trips, triage prompts            (02, 09)
  intake.rs       # intake, scale, direction, replan                     (03)
  schema.rs       # phase file types                                     (04)
  markdown.rs     # JSON → Markdown rendering, editable-Markdown parser  (01, 03)
  lint.rs         # deterministic checks                                 (05)
  audit.rs        # audit loop                                           (05)
  split.rs        # task splitting loop                                  (06)
  schedule.rs     # conflict graph, waves, ready set                     (06)
  brief.rs        # task brief composition                               (07)
  execute.rs      # scheduler tick, tree lock, verify, escalation        (07)
  overlay.rs      # per-task write buffer for the loop executor          (08)
  gates.rs        # gate evaluation                                      (09)
  issues.rs       # issue store, triage, learn                           (09)
  backend.rs      # role → backend, ask_json                             (10)
  prompts/*.md    # built-in prompts, overridable per project            (10)
```

`main.rs` gains one arm: `["build"] => forgeflow::build::run(context)`.

## New dependencies

- `schemars`: JSON Schemas for structured answers, generated from the phase types.
- `regex`: lint phrases and `diff_matches` gate triggers.
- `globset`: path-glob gate triggers and write-set patterns.

## Out of scope for v1

- A git worktree per task. The tree lock design leaves room for it later (see [07](07-execution.md#later-worktrees)).
- A non-interactive `next`/`answer` protocol for driving build mode from another agent. `next_action()` is already a pure function, so exposing it later is cheap.
- Running across several repos at once.

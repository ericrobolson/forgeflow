# 11 · Build order

v1 includes everything in this folder. The order below gets a tiny plan running end to end as early as possible (B4), then uses build mode to build the rest of itself.

Each stage lists the docs it implements and an exit criterion. A stage is done when its exit passes and `cargo test` and `cargo clippy -- -D warnings` are clean.

## B0 · Plan store and menu skeleton

- `build/` module, `["build"]` arm in `main.rs`, `USAGE` line.
- `store.rs`: folder naming, `Event`/`Envelope`, append with fsync, replay, torn-line recovery, plan lock, atomic projection writes, `.gitignore` entries ([01](01-storage.md)).
- `state.rs`: `Phase`, `PlanState`, `reduce`, `next_action` with stub steps ([02](02-state-machine.md)).
- `ui.rs`: plan list, new-plan stub, quit.

**Exit:** `forgeflow build`, then new plan "Smoke" with a blank source, shows the plan at `intake`. Kill the process, run it again, and the plan is listed with the same state. Deleting `state.json` and reopening produces identical bytes.

## B1 · Backends and structured answers

- `BackendSpec`, the role config, `Agent::ask_with` (cwd, model, timeout, stdin prompts, usage) ([10](10-backends-and-prompts.md)).
- `ask_json` with `schemars`, correction rounds, and llama-server `json_schema` for `loop`.
- The prompt registry with project overrides.

**Exit:** a test `ask_json::<ScaleProposal>` passes with both a fake CLI and a scripted `ChatBackend`. A manual run against `cli:claude` gives back a valid `ScaleProposal` for "change the README title".

## B2 · Planning: intake → approval

- `intake.rs`: scale proposal, executor pick, target repo creation, `source.md` ([03](03-scale-and-milestones.md)).
- `markdown.rs`: rendering, and the editable direction round trip ([01](01-storage.md#editable-markdown)).
- `schema.rs`: `Intake`, `Direction`, `Detail`, `Audit`, `Approval` ([04](04-phase-schemas.md)).
- `lint.rs`: direction and detail rules; `build.json` loading ([05](05-audit-and-lint.md)).
- `audit.rs`: the audit loop.
- Approval screen, hashes, and voiding approval on edit.

**Exit:** a tiny plan ("add a `--version` flag") goes from intake to approved using `cli:claude`. Editing `02_detail.json` afterwards voids the approval on the next open.

## B3 · Split and schedule

- `TaskDraft` → `Task`, the split loop, task lint rules, estimated context ([06](06-split-and-schedule.md), [05](05-audit-and-lint.md#task-rules)).
- `schedule.rs`: edges, cycle detection, waves, the ready set, static gate placement.
- Rendering `05_tasks.md`, `06_schedule.md` and `tasks/T*.md` ([07](07-execution.md#brief)).

**Exit:** the B2 plan splits into tasks that pass lint. Scheduling twice gives byte-identical files. The schedule edge-rule unit tests pass.

## B4 · Execution with `cli:*` (sequential)

- `Tree` and the tree rule, verify stages 1–8, commit per task ([07](07-execution.md)).
- The escalation ladder, retry feedback, and reports for CLI agents.
- Issues: the store, the `escalation`/`budget`/`concern` sources, and a minimal triage (promote, edit and retry, drop) ([09](09-gates-and-issues.md)).
- Wave checkpoints, the issues-first rule on open.

**Exit:** the `--version` plan runs end to end with `executor = cli:opencode:<cheap model>` (or `cli:codex:<mini>`): the tasks are verified and committed, and the plan reaches Review and then Done. A deliberately impossible task goes through cheap ×2, big ×1, and ends as an issue.

> **From here on, build mode builds itself.** B5–B7 are written as ForgeFlow plans in this repo (`_forgeflow/plans/…`), using these docs as their source.

## B5 · `loop` executor

- `WriteScope`, `Overlay`, and the `write_file`/`edit_file`/`run_check`/`report` tools; overlay-aware `read_file` and `list_directory` ([08](08-loop-write-tools.md)).
- `LoopMode::Task`, `prompts/executor.md`, and task transcripts.
- Parallel loop tasks: a context for each task, pinned slots, an `mpsc` back to the driver.

**Exit:** the same tiny plan, created again with `executor = loop`, runs end to end on a local model. A two-task plan with independent write sets runs both tasks at once, and both verify. `(s)tats` shows the two executors side by side.

## B6 · Gates, full triage, learn

- `gates.rs`: all triggers, review, refactor and check actions, default gates, `disable` ([09](09-gates-and-issues.md#gates)).
- Duplicate findings, auto-promote, `pause` issues stopping new tasks.
- Full triage (split, learn, keep for later), learned checks in lint and audit ([05](05-audit-and-lint.md#learn)).
- Human edits during a checkpoint.

**Exit:** a planted `let api_key = "sk-…"` in a task's output triggers the `secrets` gate. The security finding pauses the plan, and triage with promote produces a fix task that verifies. A learned phrase check then fires on a new task's goal.

## B7 · Scale: milestones, M00, replan

- Multiple milestones, inserting M00, the per-scale round limits and checkpoint cadence ([03](03-scale-and-milestones.md)).
- Milestone review with exit checks and claim sign-off, and replanning with history.
- Bug plans: the reproduction section, checking that the repro command fails now, and the repro becoming the first red test.

**Exit:**
- C25kBeeper runs as a `feature` plan into a new repo and reaches Done.
- GbaDecompiler runs as an `app` plan through M00 Scaffold and its review, then a replan.
- A bug plan with a failing repro command ends with that command passing.

## Rough size

| Stage | New code (est.) | Mostly |
|---|---|---|
| B0 | 600 lines | store, reducer |
| B1 | 500 | agent plumbing |
| B2 | 1,100 | schemas, markdown, lint |
| B3 | 700 | graph code |
| B4 | 1,000 | git and process handling |
| B5 | 600 | tools, threading |
| B6 | 700 | gates, triage UI |
| B7 | 600 | phase extensions |

That's about 5,800 lines, close to the size of the current codebase (around 7,000). B0–B4 are the critical path and have to be written by hand, or with your usual setup. Everything after that is built with build mode itself.

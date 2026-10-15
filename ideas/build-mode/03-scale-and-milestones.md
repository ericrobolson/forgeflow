# 03 · Scale, intake and milestones

Module: `src/build/intake.rs`

One pipeline handles every size of work. **A small plan is a large plan with one milestone.** Scale only changes how deep each phase goes.

## Scales

| | tiny | feature | app |
|---|---|---|---|
| Example | "change this web page" | Couch to 5K beeper page | GbaDecompiler, FantasyStockSim |
| Milestones | 1 | 1 | M00 Scaffold + N |
| Direction | goal + 1–3 acceptance checks | full | full + milestone outline with exits |
| Detail | invariants and simulators optional | full | full, one milestone at a time |
| Audit rounds (max) | 1 | 3 | 3 |
| Typical tasks | 1–5 | 5–30 | 20–60 per milestone |
| Checkpoints | once, at the end | every wave | every wave + milestone review |
| Replan | — | — | after every milestone |

The scale is recorded in `00_intake.json`. You can change it later from the plan menu, which appends a new `IntakeRecorded` and applies from the next milestone on.

## Intake

Input is one of:

- **An idea file path.** It is copied verbatim to `source.md`, and the name comes from the file name.
- **A typed description.** It is saved as `source.md`.
- **Blank.** You write the direction yourself.

The `planner` role reads the source plus a summary of the repo (top-level tree, README, AGENTS.md, Cargo.toml/package.json if present) and answers:

```rust
pub struct ScaleProposal {
    pub name: String,                    // PascalCase
    pub scale: Scale,                    // tiny | feature | app
    pub rationale: Vec<String>,          // shown in the UI
    pub kind: PlanKind,                  // feature | bug
    pub milestones: Vec<String>,         // names only; app ≥ 2, else exactly 1
    pub open_questions: Vec<String>,     // what the human must decide in Direction
}
```

You confirm or override the scale, set the target repo (it can be created, see [02](02-state-machine.md#terminal-ui)), and pick the executor. All of that is stored as `Intake` ([04](04-phase-schemas.md#intake)).

## Direction

The planner drafts `01_direction` from the source, putting `open_questions` at the top of the Markdown as comments. You edit it in the editor ([01](01-storage.md#editable-markdown)). This is where the human-owned work lives: goal, non-goals, architecture, constraints and the milestone outline.

How much drafting is needed depends on the source:

- **GbaDecompiler** already has goal/non-goals, architecture and Phases 0–4 with measurable exits. The draft is mostly extraction. Phases become milestones, and each exit becomes `[check]` and `[claim]` lines.
- **FantasyStockSim** is a game design doc. It has a build order but no stack, architecture or test strategy. The draft will be mostly open questions: language and engine, UI, how to test emergent behaviour (seeded simulation with property checks), and where determinism is needed.

For a **bug** plan, Direction swaps Architecture for `Reproduction` (steps, plus a command that currently fails), `Evidence` (logs, inputs, stack traces) and `Root cause` (an analysis, or "unknown"). Lint ([05](05-audit-and-lint.md)) requires the reproduction command, and Split makes it the red test of the first task.

## Milestones

Each milestone has a goal and an exit: a list of checks (commands) and claims. A claim is only allowed when no command can verify it, such as "the generated C++ reads like a human wrote it". At milestone Review the harness runs every exit check. A human signs off on the claims.

### M00 Scaffold (apps, and features in repos without a baseline)

M00 is added automatically, before the first milestone in the outline. It is planned and executed like any other milestone, and seeded with these goals:

1. The repo exists, builds and has a test runner. The language and layout come from Direction > Architecture.
2. `AGENTS.md` is written from the direction: purpose, layout, commands, invariants, conventions, and the rule that every change needs a test.
3. `_forgeflow/build.json` has `baseline` set: build, lint as errors, format check, tests ([05](05-audit-and-lint.md#buildjson)).
4. A simulator skeleton exists for each external dependency already known in Direction. For GbaDecompiler that's an mGBA trace oracle, ROM fixtures and a Prolog KB stub. For FantasyStockSim it's a seeded RNG and a fixed-calendar clock.
5. A CI-equivalent script runs the baseline (`scripts/check` or `make check`).

A **feature** plan in an existing repo gets M00 only when `build.json` has no `baseline`. In that case M00 shrinks to steps 3 and 5.

### Rolling planning

Only the current milestone goes through Detail → Split. The others stay at outline level in `01_direction`. After each milestone's Review, an app plan enters **Replan**:

- The planner gets the direction, the milestone's review, the issues opened and resolved, the `learn`ed checks, and a summary of the diffs.
- It proposes an updated direction: changed goals and exits for the remaining milestones, new milestones, or milestones dropped.
- You see a diff of `01_direction.md` and accept or edit it. The old version goes to `history/`.

That's how GbaDecompiler Phase 4 ("whatever had to change in the shared crates is the record") gets planned with real information instead of guesses.

### Milestone review

```rust
pub struct Review {
    pub exit_results: Vec<ExitResult>,    // per check: pass/fail + output tail
    pub claims: Vec<ClaimSignoff>,        // human: accepted / rejected + note
    pub notes: String,
    pub outcome: ReviewOutcome,           // complete | needs_work(Vec<IssueId>)
}
```

`needs_work` opens issues and returns the milestone to Execute once those issues become tasks. A milestone can't complete while an exit check fails.

## Worked example: C25kBeeper (feature)

Source: "implement a static webpage that plays a beep based on couch to 5k timed intervals".

- **Intake:** `feature`, 1 milestone, new repo. Open questions: which program (the standard 9-week NHS plan?), whether there's a pause button, whether it runs offline.
- **Direction:** a static HTML/TS page, no framework; non-goals: accounts, GPS.
- **Detail:**
  - *Invariants:* intervals are a typed `Schedule` of `Segment { kind: Run | Walk, seconds: u32 }`, so negative durations can't be represented and a week/day lookup is total over 1..=9 × 1..=3.
  - *External dependencies:* `AudioOut` (simulated by a recording fake), `Clock` (simulated by a manual clock).
  - *Acceptance:* `npm test -- schedule` (every week/day sums to the published total), `npm test -- player` (with the fake clock, beeps fire at every segment boundary and nowhere else), `npx playwright test smoke` (the page loads and Start begins week 1 day 1).
- **Split:** about 8 tasks: types, schedule data, fake clock and audio, player state machine, UI bindings, smoke test, AGENTS.md, README.

## Tests

- Scale proposal parsing; overrides change the audit round limit and the checkpoint cadence.
- M00 is inserted for apps, and for features only when there is no baseline.
- Replan replaces `01_direction` and moves the old version to `history/`, and completed milestones can't be changed by it.
- Review: one failing exit check blocks completion.

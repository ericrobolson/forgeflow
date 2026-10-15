# 07 · Execution

Modules: `src/build/brief.rs`, `src/build/execute.rs`

## The tree rule

> **Outside the tree lock, the working tree equals `HEAD`.** (The `_forgeflow/` folder is the exception.)

This is how parallel tasks in one working tree avoid each other's half-finished edits. Rust compiles the whole crate, so a half-edited file in task B would break the tests of task A. The rule is kept in two ways:

- **`loop` executor:** writes go into a per-task **overlay** in memory ([08](08-loop-write-tools.md)), not to disk. The overlay is written to the tree only while holding the tree lock, either to run a check or to verify, and is reverted afterwards unless the task is verified and committed.
- **`cli:*` executors** write straight to disk, so a CLI run holds the tree lock for its **whole duration**. That makes CLI execution sequential in practice, and it also covers escalated runs (the big model goes through a CLI).

```rust
pub struct Tree { lock: Mutex<()>, repo: PathBuf }
impl Tree {
    pub fn with<T>(&self, f: impl FnOnce(&TreeGuard) -> T) -> T;   // asserts clean on entry and on exit
}
```

`TreeGuard` checks on entry that `git status --porcelain -- . ':!_forgeflow'` is empty, and puts it back that way on exit: it reverts with `git checkout -- <paths>` and removes new files, touching only paths that changed since entry.

## Parallelism

| Executor | `max_parallel` |
|---|---|
| `loop` | `min(build.json.limits.max_parallel (default 4), server::SLOTS)`. Each task is pinned to its own llama-server slot through `StepOptions.slot`, which keeps that slot's prompt cache |
| `cli:*` | 1 (the tree lock is held for the whole run) |

Every running loop task gets its own `Context` with `Registers::in_memory()`, an empty stack, the shared `Project`, and the shared server endpoint. Registers and memory from your chat session are never touched.

## Brief

`brief::render(task, detail, cfg, prior: Option<&PriorAttempt>) -> String`, saved as `tasks/T014.md`. The brief is the **only** thing the executor sees. It never sees the plan.

```markdown
# T014 · Add `Provider::Anthropic` variant

You are completing one small, fully specified task in this repository. Do only this task.
If something in this brief seems wrong or underspecified, do not guess: say so in `concerns` in your report.

## Goal
…

## You may write only these files
- src/agent.rs
Any change to another file fails the task.

## Read first
- src/chat.rs (lines 44–80)

## Symbols
- `Provider::Anthropic`, `Agent::anthropic()`

## Steps
1. **Red.** Add the test `anthropic_command_uses_messages_api` in `src/agent.rs`. Run `cargo test anthropic_` and confirm it fails.
2. **Green.** Implement the smallest change that makes it pass. Run `cargo test anthropic_`.
3. **Refactor ×2.** Improve names, remove duplication, and simplify. Run `cargo test anthropic_` after each pass.
4. **Baseline.** Run: `cargo build --all-targets`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`, `cargo test`.

## Acceptance (all must pass)
- `cargo test anthropic_`: the Anthropic provider builds a Messages API request

## Out of scope
- Streaming support; changing other providers; config changes.

## Project context
<AGENTS.md, capped at context_docs_tokens>

## Report
<loop: call the `report` tool | cli: write JSON to <path>> matching this schema: …

## Previous attempt (only on retries)
Failed at: Acceptance. Output (tail): …
Decisions it made: …
```

Retries start from a **fresh** context: the brief plus the previous failure. They never inherit the previous transcript, so the context stays small.

## Running a wave

```rust
pub fn run_wave(context: &mut Context, store: &mut Store) -> Result<(), String>
```

1. Compute `ready` ([06](06-split-and-schedule.md#ready-set)). Start up to `max_parallel` tasks: append `TaskStarted { tier, executor }` and spawn a thread.
2. As each one finishes, run **verify** (below) on the driver thread, then append `TaskVerified` or `TaskFailed`, then run the dynamic gates ([09](09-gates-and-issues.md)).
3. Refill the slots from `ready`.
4. When the wave has no running and no ready tasks, append `CheckpointReached`.

Events are only ever appended from the driver thread. Workers send results back over an `mpsc` channel.

## Verify

Everything runs inside `Tree::with`, and stops at the first failure:

| # | Stage | Check |
|---|---|---|
| 1 | apply | `loop`: write the overlay into the tree. `cli`: the edits are already there |
| 2 | `Scope` | changed paths (porcelain, excluding `_forgeflow/`) ⊆ `task.writes` |
| 3 | `Budget` | changed files ≤ `max_files`; `git diff --numstat` plus new-file line counts ≤ `max_diff_lines` |
| 4 | `Red` | **red proof**: if `tdd.test_path` is a separate file, temporarily revert every *other* written file to `HEAD`, run `tdd.green`, and require it to **fail**, then restore. This proves the test actually exercises the change. Skipped, with a note, when the test lives in the same file as the code |
| 5 | `Baseline` | every `build.json.baseline` check |
| 6 | `Acceptance` | every `task.acceptance` check |
| 7 | `Executor` | the report exists and passes the schema |
| 8 | commit | `git add -- <writes> <plan dir>`; `git commit` ([01](01-storage.md#commits)) |

On failure, the tree is reverted and the failure stage plus the last 4 KB of output are recorded.

Each check runs with `sh -c`, at the repo root, with `timeout_s` (default 300). It gets these environment variables: `FORGEFLOW_TASK`, `FORGEFLOW_PLAN`, `CI=1`.

## Escalation ladder

| Attempt | Tier | Backend | Input |
|---|---|---|---|
| 1 | cheap | the plan's `executor` | brief |
| 2 | cheap | the plan's `executor` | brief + failure 1 |
| 3 | big | role `escalation` (a CLI, e.g. `cli:claude`) | brief + failures 1–2 + both reports |
| — | — | — | open an issue (`source: escalation`, `severity: major`), and block the task's dependents |

Exceptions:

- **`Budget` failure:** the ladder is skipped and a `budget` issue opens right away, with the default triage action set to *split*. A larger model wouldn't fix a task that's too big.
- **`Interrupted` attempts** don't count.
- **`Scope` failure on attempt 1:** retried, with the out-of-scope paths named in the feedback.

## Reports

- `loop`: the executor calls a `report` tool whose arguments are the `Report` schema. That ends the loop; plain-text answers don't.
- `cli:*`: the brief says to write JSON to `milestones/<M>/reports/T014.attempt-<n>.json`. The harness reads it after the run, and copies the one from the verified attempt to `reports/T014.json`.

`concerns` in the report become `minor` issues (`source: concern`) whether or not the task is verified.

## Later: worktrees

The tree rule makes this a local change. `Tree::with` would hand out a fresh worktree per task instead of holding a lock, and verify would merge or rebase on success. CLI executors could then run in parallel too. Nothing outside `execute.rs` would need to change.

## Tests

- Tree rule: after any verify path (pass, each failure stage, panic inside the closure), `git status` is clean.
- A scope violation is detected and reverted. The budget counts new files.
- Red proof: a test that passes without the implementation fails the stage.
- The ladder: the attempts sequence, budget short-circuit, and interrupted attempts not counting.
- Parallel: two loop tasks with scripted backends that write disjoint files both verify, and are committed in the order they finish.
- Rendering a brief: snapshot test, with and without a previous attempt.

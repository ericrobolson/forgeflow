# 09 · Gates, issues and checkpoints

Modules: `src/build/gates.rs`, `src/build/issues.rs`

## Gates

A gate is a rule that fires on a trigger and does one thing: run a command, run a model review, add a task, or pause for a human. Gates live in `build.json` (`gates`) on top of built-in defaults.

```json
"gates": [
  { "id": "secrets",  "when": { "diff_matches": "(?i)(api[_-]?key|secret|token|password|bearer)" },
                      "do": { "review": "security" }, "pause_on_findings": true },
  { "id": "unsafe",   "when": { "diff_matches": "\\bunsafe\\b" }, "do": { "review": "security" } },
  { "id": "net",      "when": { "paths": ["src/server.rs", "src/**/http*.rs"] }, "do": { "review": "security" } },
  { "id": "review5",  "when": { "every_n_tasks": 5 }, "do": { "review": "general" } },
  { "id": "refactor", "when": { "wave_end": true },   "do": { "refactor": { "max_tasks": 2 } } },
  { "id": "audit",    "when": { "tags": ["security"] }, "do": { "check": { "claim": "cargo audit clean", "cmd": "cargo audit" } } }
]
```

```rust
pub struct Gate { pub id: String, pub when: Trigger, pub do_: GateAction, pub pause_on_findings: bool }
pub enum Trigger {
    EveryNTasks(u32), WaveEnd, MilestoneEnd,
    Paths(Vec<String>),        // globs, matched against the verified task's writes
    Tags(Vec<String>),         // matched against task.tags
    DiffMatches(String),       // regex over the verified commit's added lines
}
pub enum GateAction {
    Check(Check),              // deterministic; failure opens a major issue
    Review { prompt: String }, // role "reviewer" (or "security" when prompt == "security")
    Refactor { max_tasks: u32 },
    Human,                     // extra checkpoint
}
```

Built-in defaults, which `build.json` can turn off with `"disable": ["<id>"]`:

- `secrets`
- `unsafe` (Rust repos only)
- `review5`
- `refactor` at wave end

### When gates run

- `Paths`, `Tags` and `DiffMatches` are checked **for each verified task**, against that task's commit.
- `EveryNTasks` is checked after each verify, against the milestone's count of verified tasks.
- `WaveEnd` runs just before `CheckpointReached`. `MilestoneEnd` runs before Review.

A gate that fires appends `GateFired`. Several gates firing on the same anchor run in the order they're listed in config.

### Review gates

The reviewer gets: the diffs of the commits covered (that task for task-anchored gates; every commit since the last run of this gate for count/wave gates), the briefs of those tasks, the detail's invariants and risks, and `prompts/review-<prompt>.md`. It answers:

```rust
pub struct ReviewAnswer { pub findings: Vec<FindingDraft> }
pub struct FindingDraft { pub title: String, pub body: String, pub severity: Severity, pub security: bool,
                          pub task: Option<TaskId>, pub paths: Vec<String>, pub suggested_fix: Option<String> }
```

Each finding becomes an issue (`source: review(<gate id>)`). Findings are de-duplicated by `sha256(normalized title + sorted paths)` against all issues in the plan, including dropped ones, so the same finding isn't raised twice.

### Refactor gates

The splitter role receives the files touched in the anchor's range and is asked for up to `max_tasks` `refactor` tasks. Those need `symbols`, and their acceptance is the existing tests (behaviour must not change). They are added as `TaskAdded` with `origin: gate(refactor)`, so they go through the usual lint and scheduling.

## Issues

`issues.json`. The schema is in [04](04-phase-schemas.md#issues).

### Sources and defaults

| Source | Severity | `pause` | Default triage action |
|---|---|---|---|
| review, security finding | from the finding | **yes** | promote |
| review, other finding | from the finding | no | — |
| escalation (ladder exhausted) | major | no (dependents are blocked) | edit brief and retry |
| budget | major | no (dependents are blocked) | split |
| split_failed | major | no (dependents are blocked) | edit brief / split |
| concern (from a report) | minor | no | — |
| interrupted_diff (resume) | minor | no | drop |
| human | chosen | chosen | — |

### Auto-promote

`build.json`:

```json
"auto_promote": { "security": true, "min_severity": "blocker" }
```

Matching issues become tasks immediately, through the splitter. They are still shown at the next triage, marked *auto-promoted*.

### Triage

Triage runs whenever `next_action` returns `Triage` ([02](02-state-machine.md#reducer-and-next-action)): every open issue when a plan is selected, and `pause` issues as soon as they appear.

```
I007  major  escalation  T031 "parse jump tables in THUMB mode"
      failed: cheap ×2 (Acceptance), big ×1 (Acceptance)
      last: test jump_table_thumb_bx … expected 12 entries, found 0
(p)romote · (e)dit brief and retry · (s)plit · (d)rop · (l)earn · (v)iew · (k)eep for later
```

| Action | Effect |
|---|---|
| promote | the splitter turns the issue into tasks (`TaskAdded`, `origin: issue`); issue → `promoted` |
| edit brief and retry | opens `tasks/T031.md`, and saves your edits as a new version of the task's `goal`/`out_of_scope`/`acceptance`; resets the attempt counter; issue → `promoted([T031])` |
| split | sends the task to the splitter, which replaces it with smaller tasks; dependents are moved onto the new tasks |
| drop | needs a reason; dependents of a dropped task open their own issues |
| learn | drafts a learned check from the issue ([05](05-audit-and-lint.md#learn)); can be combined with any other action |
| keep for later | `deferred`; doesn't block; shown again at milestone review |

## Checkpoints

The plan supervises **per wave**:

1. When the wave finishes, the wave-end gates run and `CheckpointReached` is appended.
2. The checkpoint screen ([02](02-state-machine.md#checkpoint-screen)) lists each task with its diffstat, report summary and decisions, the gate results, and the new issues.
3. You can view diffs (`git show <commit>`), read reports, and triage issues.
4. `(c)ontinue` appends `CheckpointCleared`, and the next wave can start.

While at a checkpoint you can also make code changes yourself. On continue, any uncommitted change is committed as `human: <message>`, and you're asked whether it affects any pending task. If it does, those tasks' `reads` are flagged so the next scheduling run checks them again.

A `pause` issue (security) stops *starting* new tasks immediately. Running tasks finish and are verified. Triage is then forced before the wave goes on.

## Tests

- Each trigger type fires on a matching anchor and stays quiet on a non-matching one.
- `DiffMatches` only looks at added lines.
- Duplicate findings are dropped across reviews.
- Auto-promote produces `TaskAdded` events. A security finding pauses the plan, and no new task starts until it's triaged.
- Every triage action, checked against the resulting events.
- A checkpoint with human edits creates a `human:` commit and flags the affected tasks.

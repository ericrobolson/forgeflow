# 06 · Split and schedule

Modules: `src/build/split.rs`, `src/build/schedule.rs`

## Split

The `splitter` role turns an approved detail into tasks that each fit one cheap-model context window.

**Input:** the direction (this milestone only), the detail, the repo summary, the current contents (or an outline, if large) of every path in `detail.files`, the `build.json` limits, the executor's context size, and the split guidance in `prompts/split.md`.

**Output:**

```rust
pub struct SplitAnswer { pub tasks: Vec<TaskDraft> }
pub struct TaskDraft { pub key: String, pub depends_on: Vec<String> /* keys */, /* …Task fields minus harness-owned ones (04) */ }
```

### Guidance the splitter follows (`prompts/split.md`)

- **Order by kind:** `encode_invariant` and `simulator` tasks first, so later tasks are written against types and fakes that already exist. Then `implement`/`test`, then `docs`.
- **One behaviour per task.** If the goal needs "and", it's two tasks.
- **Every task is red → green → refactor.** `tdd.red` must fail before the change and `tdd.green` must pass after it, usually as the same command.
- **Name things.** Give exact file paths, exact symbols, and exact test names inside the check commands.
- **No design decisions are left to the executor.** If a choice exists, make it in the goal.
- **`out_of_scope` lists the nearby things the executor will be tempted to touch.**

### The loop

1. Ask the splitter. Check the answer against the schema.
2. Assign IDs: the next free `T<NNN>`, in the order of the answer. Turn key references into IDs, and compute `est_ctx_tokens`.
3. Run `lint_task` on every task ([05](05-audit-and-lint.md#task-rules)).
4. If any fail, send **only the failing drafts** back, each with its findings. Each one can be replaced by one or more drafts with new keys. Dependencies on a replaced key move to *all* of its replacements.
5. Repeat until the round limit. Tasks that still fail become `split_failed` issues. Their dependents are `Blocked` on that issue.
6. Append `TasksRecorded`.

Steps 2–5 are deterministic. Only steps 1 and 4 call the model.

## Schedule

Fully deterministic; no model involved.

### Edges

Each rule adds an edge, and every edge records its reason:

| Rule | Edge |
|---|---|
| Declared | each `depends_on` |
| `WriteConflict(path)` | two tasks write `path`: lower ID → higher ID |
| `ReadAfterWrite(path)` | A writes `path` and B reads it: A → B, unless B → A is already implied (then nothing is added) |
| `Interface(key)` | the task in whose `owns_interfaces` the key appears → every task that has `key` in `interface_keys` |
| `Simulator(dep)` | the `simulator` task for `dep` → every task tagged `external:<dep>` |

Every edge points from the task that must finish first to the one that waits. Paths are compared after `Project::resolve`, so `./src/a.rs` and `src/a.rs` are the same path. A `reads` entry with a line range conflicts with the whole file.

Because every pair of tasks that touch the same file (write/write or write/read) gets an edge, two running tasks can never touch the same file. That's the file-lock rule, enforced by the graph instead of by runtime locks.

### Cycles

Kahn's algorithm. If a cycle remains, the tasks in it and their edge reasons go back to the splitter as one more split round ("these tasks form a cycle through …; restructure them"). If that doesn't fix it, a blocking `split_failed` issue is opened.

### Waves

`wave(t) = 1 + max(wave(p) for p in preds(t))`, and 1 for tasks with no predecessors. Waves are the checkpoint boundaries ([09](09-gates-and-issues.md#checkpoints)).

- **Within a wave**, the ready set is dynamic. A task starts as soon as its predecessors are verified and a slot is free.
- **Across waves**, the checkpoint is a barrier. No task from wave N+1 starts until checkpoint N is cleared.

### Ready set

```rust
pub fn ready(schedule: &Schedule, exec: &Execution, wave: u32) -> Vec<TaskId>;
```

A task is ready when: it is in the current wave, its status is `Pending`/`Ready`, every predecessor is `Verified`, and no `pause` issue is open. Ready tasks are ordered by ID. The executor takes up to `max_parallel` of them ([07](07-execution.md#parallelism)).

### Tasks added mid-milestone

Promoted issues and gate-generated tasks (refactors, review fixes) are added with `TaskAdded`. Scheduling runs again over the **unstarted** tasks only. Verified and running tasks keep their waves. New tasks get `wave = max(current_wave + 1, from_edges)`, so they never slip into a wave that has already been checkpointed.

### Gate placement

Gates with static triggers (`wave_end`, `milestone_end`, `paths`, `tags`) are placed onto anchors here and recorded in `Schedule.gates`. Gates with dynamic triggers (`every_n_tasks`, `diff_matches`) are checked as tasks are verified ([09](09-gates-and-issues.md)).

## Rendered output

- `05_tasks.md`: a table of ID, title, kind, writes, depends_on and wave.
- `06_schedule.md`: waves as lists, a Mermaid `graph TD` of edges labelled with their reasons, and placed gates.
- `tasks/T014.md`: the brief ([07](07-execution.md#brief)).

## Tests

- Each edge rule on its own, plus the path normalization cases.
- A cycle goes back to the splitter. A cycle that remains becomes an issue.
- Waves for a diamond, a chain, and disconnected parts.
- Snapshot: scheduling the same tasks twice gives identical output.
- Adding a task mid-milestone never changes a verified task's wave and never lands in a wave that has been checkpointed.
- Split: replacing one draft with three moves its dependents onto all three. Tasks that fail lint after the round limit become issues, and their dependents are blocked.

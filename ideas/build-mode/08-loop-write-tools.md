# 08 · Write tools for ForgeFlow's loop

Modules: `src/build/overlay.rs`; changes in `src/tools.rs`, `src/ops.rs`, `src/workflow.rs`, `src/agent_loop.rs`

The `loop` executor runs ForgeFlow's own agent loop (`agent_loop::run_with`) against a local model. Today that loop can't write files: its tools are `read_file`, `list_directory`, `reg_view`, `reg_search`, `reg_edit`, `reg_store`, `emit` and `make-image`. Build mode needs writing, limited to the task's write set, buffered so the tree rule holds ([07](07-execution.md#the-tree-rule)), and able to run checks.

## Write scope on `Context`

```rust
// workflow.rs
pub struct Context {
    // …existing fields…
    pub write_scope: Option<WriteScope>,
}

// build/overlay.rs
pub struct WriteScope {
    pub allowed: BTreeSet<PathBuf>,          // resolved task.writes
    pub overlay: Overlay,
    pub checks: CheckSet,                    // red, green, acceptance[], baseline[]
    pub tree: Arc<Tree>,                     // for run_check
    pub report: Option<Report>,              // set by the `report` tool
}
pub struct Overlay { files: BTreeMap<PathBuf, String> }
```

With `write_scope: None` (chat, `.ff` programs), the write tools aren't offered at all, and nothing about chat changes. The write-queue words in TODO.md (`queue_write`/`pending`/`approve`/`apply`) can later reuse `Overlay` as their queue.

## Tools

These are added to `tools::toolset()` only when `context.write_scope.is_some()`:

| Tool | Args | Behaviour | Result |
|---|---|---|---|
| `write_file` | `path`, `content` | path must be in `allowed`; sets the overlay entry. `content` accepts `$R5` | inline: `wrote src/a.rs (overlay, 120 lines)` |
| `edit_file` | `path`, `old`, `new` | applies to the overlay copy, or the disk copy if there's no overlay copy; `old` must match **exactly once** | inline: `edited src/a.rs: 1 replacement` or an error naming the match count |
| `run_check` | `which`: `red` \| `green` \| `acceptance` \| `baseline` | `Tree::with`: apply the overlay → run the checks → revert | register: full output; the receipt shows `PASS`/`FAIL` plus the first failing line |
| `report` | the `Report` fields | checks against the schema, stores it, and **ends the loop** | — |

Changes to existing tools when a scope is set:

- `read_file` returns the overlay copy if there is one, and marks it in the receipt with `(overlay)`.
- `list_directory` includes files that exist only in the overlay.

Path errors give the model what it needs to correct itself:
`error: src/main.rs is not in this task's write set. You may write: src/agent.rs`.

### Why these four tools

- `edit_file` with an exact single match is the format that small models use most reliably, and its failures are easy to explain.
- `write_file` plus `$R` refs lets a model build a file in a register with `reg_edit` and then commit it, without repeating the content in its output. That fits the existing register design.
- `run_check` takes a check **name**, not a shell command, so the model can't run arbitrary commands. The checks come from the brief.

## Loop changes

`agent_loop::run_with` gains a mode:

```rust
pub enum LoopMode { Answer, Task { max_steps: u32 } }
```

- **System prompt.** In `Task` mode, the chat-agent prompt is replaced with `prompts/executor.md`. It covers register usage (unchanged), the write rules, red → green → refactor, *"call `report` when done; plain text does not end the task"*, and *"if blocked, call `report` with a concern"*.
- **Ending.** In `Task` mode, a turn with no tool calls doesn't end the loop. The model gets a nudge: `call report to finish`. Two nudges in a row end the attempt with failure stage `Executor`.
- `MAX_REPEATED_FAILURES` and the step limit work as today. `max_steps` comes from `task.budget.max_steps`.
- Transcripts go to `milestones/<M>/transcripts/T014.attempt-<n>.json` instead of `_forgeflow/sessions/`.

## Model requirements

`loop` needs a model with `tool_calls: true` in the catalog. That's checked already in `agent_loop::run`, and also at intake when the executor is picked. Intake also warns if `config.ctx` is under 16k: briefs, context docs and tool schemas don't fit in 8k with room left to work.

## Tests

- Path guard: allowed, not allowed, `..` escape, and a symlink escape (reuses `Project::resolve`).
- `edit_file`: zero, one and two matches; edits stack on the overlay copy.
- `read_file` returns the overlay copy after a write.
- `run_check` leaves the tree clean afterwards, and its receipt shows PASS/FAIL.
- Task mode: plain text gets a nudge, and the `report` tool ends the loop with a parsed `Report`.
- With no scope set, the tool list is unchanged (a regression guard for chat).

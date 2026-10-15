# 01 · Storage

Module: `src/build/store.rs`, `src/build/markdown.rs`

## Plan folder

Plans live in the target repo:

```
<repo>/_forgeflow/plans/261010-143512_GbaDecompiler/
  state.json                 # rebuilt from events: phase, scale, executor, current milestone, cursor
  events.jsonl               # append-only source of truth
  00_intake.json  00_intake.md
  01_direction.json  01_direction.md
  source.md                  # verbatim copy of the idea file or description, if any
  issues.json  issues.md      # rebuilt from events
  milestones/
    M00_Scaffold/
      02_detail.json   02_detail.md
      03_audit.json    03_audit.md
      04_approval.json
      05_tasks.json    05_tasks.md
      tasks/T001.md …          # rendered briefs (07)
      06_schedule.json 06_schedule.md
      07_execution.json 07_execution.md
      08_review.json   08_review.md
      reports/T001.json …      # executor reports
      transcripts/             # ignored by git
    M01_SkeletonAndScoring/ …
  history/                   # earlier versions of documents replaced by a later phase
  .lock                      # ignored by git
```

### Naming

- Folder: `YYMMDD-HHMMSS_<Name>` in local time, matching `~/dev/forge/ideas/`. `<Name>` is PascalCase. It comes from the idea file's name after its timestamp when there is one (`261010-072929_GbaDecompiler.md` → `GbaDecompiler`), otherwise from the title the intake step proposes and you confirm.
- Milestones: `M<NN>_<PascalName>`, zero-padded.
- Tasks: `T<NNN>`, numbered across the whole plan rather than per milestone, so an issue or report can name a task without ambiguity.
- Issues: `I<NNN>`. Findings: `F<NNN>`.

### What goes in git

Everything is tracked except `.lock` and `transcripts/`. `Project::initialize` adds those entries to `_forgeflow/plans/.gitignore`. `events.jsonl` is tracked because it is the truth. It only ever grows, so its diffs stay readable.

## Three kinds of file

| Kind | Examples | Written by | Mutable? |
|---|---|---|---|
| **Log** | `events.jsonl` | `Store::append` only | append-only |
| **Documents** | `00`–`04`, `08`, `source.md` | the phase that produces them, through an event that carries the content | replaced only when a phase runs again; the old copy moves to `history/<file>.<seq>` |
| **Projections** | `state.json`, `05`–`07`, `issues.*`, `tasks/*.md` | `Store::project()` after each append | rewritten from events every time |

Every document and projection has a JSON file (the truth) and a Markdown rendering. Markdown is never parsed back, with one exception: the human-editable documents (see [Editable Markdown](#editable-markdown)).

## Events

One JSON object per line:

```json
{"seq":42,"at":"2026-10-10T14:41:07Z","event":"task_verified","task":"T014","commit":"a1b2c3d","attempt":1}
```

```rust
#[derive(Serialize, Deserialize)]
pub struct Envelope { pub seq: u64, pub at: String, #[serde(flatten)] pub event: Event }

#[derive(Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    PlanCreated { name: String, source: Option<String> },
    IntakeRecorded { intake: Intake },
    DirectionRecorded { direction: Direction },
    MilestoneStarted { milestone: MilestoneId },
    DetailRecorded { milestone: MilestoneId, detail: Detail },
    AuditRound { milestone: MilestoneId, round: AuditRound },
    Approved { milestone: MilestoneId, approval: Approval },
    ApprovalVoided { milestone: MilestoneId, reason: String },
    TasksRecorded { milestone: MilestoneId, tasks: Vec<Task> },
    TaskAdded { task: Task },                       // from promoted issues
    Scheduled { milestone: MilestoneId, schedule: Schedule },
    TaskStarted { task: TaskId, attempt: u32, executor: BackendSpec },
    TaskReported { task: TaskId, attempt: u32, report: Report },
    TaskVerified { task: TaskId, attempt: u32, commit: String },
    TaskFailed { task: TaskId, attempt: u32, failure: Failure },
    TaskInterrupted { task: TaskId, attempt: u32 },
    GateFired { gate: String, after: GateAnchor },
    IssueOpened { issue: Issue },
    IssueResolved { issue: IssueId, resolution: Resolution },
    CheckLearned { check: LearnedCheck },
    CheckpointReached { milestone: MilestoneId, wave: u32 },
    CheckpointCleared { milestone: MilestoneId, wave: u32, note: Option<String> },
    MilestoneReviewed { milestone: MilestoneId, review: Review },
    Replanned { direction: Direction },
    PlanCompleted,
}
```

Events carry the full content of the documents they produce: `DetailRecorded` holds the whole `Detail`. Replaying the log therefore rebuilds every file, with no outside input.

### Store API

```rust
pub struct Store { dir: PathBuf, _lock: PlanLock, events: Vec<Envelope> }

impl Store {
    pub fn list(project: &Project) -> Result<Vec<PlanSummary>, String>;   // reads state.json only
    pub fn create(project: &Project, name: &str, source: Option<&Path>) -> Result<Store, String>;
    pub fn open(project: &Project, dir_name: &str) -> Result<Store, String>; // lock + replay
    pub fn append(&mut self, event: Event) -> Result<&PlanState, String>;    // fsync, reduce, project
    pub fn state(&self) -> &PlanState;
}
```

`append` writes the line with `O_APPEND`, calls `fsync`, applies `state::reduce` and calls `project()`. Projection files are written as `file.tmp` and then renamed into place. If the process dies between the append and the projection, the next `open` replays the log and rewrites the projections.

## Locks

- **Plan lock** (`.lock`): created with `create_new` and holding the pid and hostname. If it already exists and that pid is not running, the lock is stale: report it and take it over. One `forgeflow build` process runs a plan at a time. Parallel tasks are threads inside that process, so no cross-process lease is needed.
- **Tree lock**: in memory, a `Mutex<()>` owned by the executor ([07](07-execution.md)).

## Resume

`Store::open`:

1. Take the plan lock.
2. Read `events.jsonl`. A torn final line (a crash mid-write) is cut off, and a warning is logged.
3. Replay the events into `PlanState`, then rewrite every projection whose bytes differ.
4. For each task with `TaskStarted` but no later verified, failed or interrupted event, append `TaskInterrupted`. Interrupted attempts don't count toward the escalation ladder.
5. Check the working tree:
   - `git status --porcelain`, ignoring `_forgeflow/`, must be clean.
   - If it isn't and the changes fall inside an interrupted task's write set, offer: *discard* (`git checkout`/`git clean` those paths) or *keep for review* (open an issue holding the diff, then discard).
   - Changes anywhere else are yours: offer to commit them as `human: <message>` or stop.

## Commits

Every verified task is one commit: `git add -- <write set> <plan dir>`, then `git commit -m "T014: <title> [forgeflow:<plan>]"`. Phase transitions that don't touch code (approval, checkpoints) commit only the plan folder. The git history then mirrors the event log, and `git revert` of one task is clean.

## Determinism

- Use `BTreeMap` and sorted `Vec`s everywhere in schema types. Serialize with `serde_json::to_string_pretty` plus a trailing newline.
- Sort IDs as numbers. They are zero-padded, so they also sort correctly as text.
- No timestamps, durations or pids in documents or projections. Those go only in events.
- Markdown is rendered by hand-written functions in `markdown.rs` (no template engine), and is snapshot-tested.

## Editable Markdown

People edit `01_direction.md` (and `00_intake.md` for scale overrides) in `$VISUAL`/`$EDITOR`. If neither is set, ForgeFlow prints the path and waits for Enter, so editing in Zed works. These files use fixed headings:

```markdown
# Direction: GbaDecompiler

## Goal
…free text…

## Non-goals
- …

## Architecture
…free text…

## Constraints
- …

## Milestones
### M01 SkeletonAndScoring
Goal: …
Exit:
- [check] cargo test -p score -- kh_com_boundaries
- [claim] function-boundary precision/recall recorded in scores/kh_com.jsonl
```

`markdown::parse_direction` accepts exactly these headings. An unknown heading, a missing required section or a malformed `Exit` line is a parse error. On error, the file is reopened with an HTML comment at the top listing each error. Free-text sections keep their bytes exactly, so editing and saving without changes is a no-op.

## Tests

- Append, reopen and confirm the state is identical. Delete all projections, reopen, and confirm the bytes are identical.
- A torn last line is cut off and the plan still opens.
- A stale lock is taken over, while a held lock (the current pid) is refused.
- A task that was started and never finished shows up as `TaskInterrupted` after reopening.
- The direction Markdown round-trips byte-identical. Every malformed variant gives a located error.
- Rendering snapshots for each projection.

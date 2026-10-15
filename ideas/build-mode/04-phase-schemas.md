# 04 · Phase schemas

Module: `src/build/schema.rs`

Every type derives `Serialize, Deserialize, JsonSchema, Clone, PartialEq, Debug`. The `JsonSchema` derive (via `schemars`) produces the answer format sent to the model ([10](10-backends-and-prompts.md)). Collections are `Vec` sorted by id, or `BTreeMap`.

Models answer with *drafts*: the same shapes without the fields the harness assigns (ids, origins, statuses). Where those differ, the draft type is noted.

## Shared

```rust
pub type MilestoneId = String;   // "M01"
pub type TaskId = String;        // "T014"
pub type IssueId = String;       // "I007"

/// Something that can be verified by running it. `cmd` runs via `sh -c` at the repo root.
pub struct Check { pub claim: String, pub cmd: String, pub timeout_s: Option<u32> }

/// An exit item: a check when one is possible, a claim (signed off by a human) when not.
pub enum Exit { Check(Check), Claim { claim: String, why_not_checkable: String } }
```

## Intake

`00_intake.json`

```rust
pub struct Intake {
    pub name: String,
    pub scale: Scale,                 // tiny | feature | app
    pub kind: PlanKind,               // feature | bug
    pub target_repo: PathBuf,         // absolute when recorded; must contain _forgeflow
    pub executor: BackendSpec,        // loop | cli:<agent>[:<model>]  (10)
    pub source_file: Option<String>,  // "source.md" when present
    pub proposal: ScaleProposal,      // what the model suggested, kept for the record
}
```

## Direction

`01_direction.json`, edited by people through Markdown

```rust
pub struct Direction {
    pub goal: String,
    pub non_goals: Vec<String>,
    pub architecture: Option<String>,      // feature plans
    pub bug: Option<BugReport>,            // bug plans
    pub constraints: Vec<String>,
    pub milestones: Vec<MilestoneOutline>,
}
pub struct BugReport { pub reproduction: Vec<String>, pub repro_cmd: String, pub evidence: Vec<String>, pub root_cause: String }
pub struct MilestoneOutline { pub id: MilestoneId, pub name: String, pub goal: String, pub exit: Vec<Exit>, pub status: MilestoneStatus }
```

## Detail

`02_detail.json`, one per milestone

```rust
pub struct Detail {
    pub scope_complete_when: Vec<Exit>,     // usually the milestone's exit, refined
    pub out_of_scope: Vec<String>,
    pub invariants: Vec<Invariant>,
    pub external_deps: Vec<ExternalDep>,
    pub interfaces: Vec<Interface>,         // named seams; become interface keys (06)
    pub acceptance: Vec<Check>,             // every one is runnable
    pub files: Vec<FileIntent>,             // areas expected to change
    pub risks: Vec<Risk>,
    pub context_docs: Vec<String>,          // repo paths given to every brief, e.g. AGENTS.md
}

pub struct Invariant {
    pub id: String,                         // "INV-3"
    pub statement: String,
    pub enforcement: Enforcement,           // type | test | assert
    pub how: String,                        // "newtype Port(u16) with TryFrom; no pub constructor"
    pub why_not_type: Option<String>,       // required unless enforcement == type
}
pub struct ExternalDep { pub name: String, pub used_for: String, pub simulator: Simulator }
pub struct Simulator { pub kind: SimKind /* fake | fixtures | stub_server */, pub description: String, pub fidelity_check: Option<Check> }
pub struct Interface { pub key: String, pub description: String, pub owner_path: String }
pub struct FileIntent { pub path: String, pub change: String }      // path may be a glob
pub struct Risk { pub risk: String, pub mitigation: String, pub security: bool }
```

## Audit

`03_audit.json`

```rust
pub struct Audit { pub rounds: Vec<AuditRound>, pub verdict: AuditVerdict }   // pass | escalated_to_human
pub struct AuditRound {
    pub round: u32,
    pub lint: Vec<LintFinding>,                 // (05)
    pub findings: Vec<AuditFinding>,
    pub resolution: Option<String>,             // planner's note on how the next detail addresses them
}
pub struct AuditFinding {
    pub id: String,                             // "A2-4" = round 2, finding 4
    pub category: AuditCategory,                // gap | error | security | ambiguity | cheap_fit | untestable
    pub severity: Severity,                     // blocker | major | minor
    pub section: String,                        // "invariants[INV-3]"
    pub text: String,
}
```

## Approval

`04_approval.json`

```rust
pub struct Approval { pub direction_sha256: String, pub detail_sha256: String, pub note: Option<String> }
```

Hashes are taken over the canonical JSON bytes of the projection files, not the Markdown.

## Tasks

`05_tasks.json`. This is the full set for the milestone, including tasks added later from issues.

```rust
pub struct Task {
    pub id: TaskId,
    pub milestone: MilestoneId,
    pub title: String,
    pub kind: TaskKind,              // scaffold | encode_invariant | simulator | implement | test | refactor | docs
    pub goal: String,                // one paragraph; lint forbids vague phrases (05)
    pub writes: Vec<String>,         // exact paths (new files allowed); no globs
    pub reads: Vec<String>,          // paths, optionally "path:START-END"
    pub symbols: Vec<String>,        // named functions/types the task creates or changes
    pub interface_keys: Vec<String>, // from Detail.interfaces
    pub owns_interfaces: Vec<String>,// keys this task defines; others with the key depend on it
    pub depends_on: Vec<TaskId>,
    pub tdd: Tdd,
    pub acceptance: Vec<Check>,      // ≥ 1; includes tdd.green
    pub out_of_scope: Vec<String>,
    pub tags: Vec<String>,           // drive gates: "security", "secrets", "unsafe", "external:<dep>"
    pub budget: Budget,
    pub origin: Origin,              // plan(section) | issue(IssueId) | gate(name)
}
pub struct Tdd { pub test_path: String, pub red: Check, pub green: Check, pub refactor_passes: u8 }
pub struct Budget { pub max_files: u8, pub max_diff_lines: u32, pub est_ctx_tokens: u32, pub max_steps: u32 }
```

`TaskDraft` leaves out `id`, `milestone`, `budget.est_ctx_tokens` (computed) and `origin`. Drafts refer to each other by `key` (a short slug), which the harness turns into `depends_on` ids.

## Schedule

`06_schedule.json`

```rust
pub struct Schedule {
    pub edges: Vec<Edge>,                  // from → to, with reason
    pub waves: Vec<Vec<TaskId>>,           // display + checkpoint boundaries
    pub gates: Vec<PlacedGate>,            // (09)
}
pub struct Edge { pub from: TaskId, pub to: TaskId, pub reason: EdgeReason }
pub enum EdgeReason { Declared, WriteConflict(String), ReadAfterWrite(String), Interface(String), Simulator(String) }
```

## Execution

`07_execution.json`, rebuilt from events

```rust
pub struct Execution { pub tasks: BTreeMap<TaskId, TaskRun>, pub wave: u32 }
pub struct TaskRun { pub status: TaskStatus, pub attempts: Vec<Attempt>, pub commit: Option<String> }
pub enum TaskStatus { Pending, Ready, Running, Verified, Failed, Blocked(IssueId), Dropped }
pub struct Attempt { pub n: u32, pub tier: Tier /* cheap | big */, pub executor: BackendSpec, pub outcome: AttemptOutcome }
pub enum AttemptOutcome { Verified, Failed(Failure), Interrupted }
pub struct Failure { pub stage: FailStage, pub detail: String }   // tail of output, ≤ 4 KB
pub enum FailStage { Executor, Scope, Budget, Red, Baseline, Acceptance, Gate(String) }
```

## Report

`reports/T014.json`, written by the executor. The schema is required, and a missing or invalid report fails the attempt at stage `Executor`.

```rust
pub struct Report {
    pub summary: String,
    pub steps: Vec<ReportStep>,        // ordered: what was done, and why
    pub decisions: Vec<Decision>,      // any choice not dictated by the brief
    pub checks_run: Vec<CheckRun>,     // claim, cmd, passed, when (red|green|refactor)
    pub refactors: Vec<String>,        // what each refactor pass changed
    pub concerns: Vec<String>,         // become minor issues automatically
}
pub struct Decision { pub question: String, pub choice: String, pub why: String }
```

A non-empty `decisions` list is a signal: either the brief was missing something, or the executor went beyond it. Each decision is shown at the checkpoint, and the audit can `learn` from the pattern.

## Issues

`issues.json`. Details in [09](09-gates-and-issues.md).

```rust
pub struct Issue {
    pub id: IssueId,
    pub source: IssueSource,      // review(gate) | escalation | split_failed | budget | concern | human | interrupted_diff
    pub severity: Severity,
    pub security: bool,
    pub pause: bool,              // blocks execution until resolved
    pub task: Option<TaskId>,
    pub title: String,
    pub body: String,
    pub status: IssueStatus,      // open | promoted(Vec<TaskId>) | dropped(reason) | deferred
}
```

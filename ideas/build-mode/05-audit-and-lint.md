# 05 · Audit and lint

Modules: `src/build/lint.rs`, `src/build/audit.rs`

There are two layers. **Lint** is deterministic, cheap and runs first. **Audit** is the big model, and it only sees a detail that already passes lint.

## `build.json`

`<repo>/_forgeflow/build.json` is committed and shared by every plan in the repo. Missing fields fall back to built-in defaults.

```json
{
  "baseline": [
    { "claim": "builds",        "cmd": "cargo build --all-targets" },
    { "claim": "lint clean",    "cmd": "cargo clippy --all-targets -- -D warnings" },
    { "claim": "formatted",     "cmd": "cargo fmt --check" },
    { "claim": "tests pass",    "cmd": "cargo test" }
  ],
  "context_docs": ["AGENTS.md"],
  "limits": {
    "max_files": 2,
    "max_diff_lines": 150,
    "ctx_fraction": 0.4,
    "context_docs_tokens": 3000,
    "max_steps": 24,
    "max_parallel": 4
  },
  "vague_phrases": ["as needed", "appropriately", "handle", "etc", "TBD", "clean up",
                    "refactor as necessary", "similar to", "and so on", "properly", "as appropriate",
                    "where relevant", "if needed", "robust", "best practices"],
  "checks": [],
  "gates": []
}
```

`checks` holds the learned checks ([below](#learn)). `gates` is covered in [09](09-gates-and-issues.md).

## Lint

```rust
pub struct LintFinding { pub rule: String, pub target: String, pub message: String, pub severity: Severity }

pub fn lint_direction(d: &Direction, scale: Scale) -> Vec<LintFinding>;
pub fn lint_detail(d: &Detail, repo: &Project, cfg: &BuildConfig) -> Vec<LintFinding>;
pub fn lint_task(t: &Task, all: &[Task], detail: &Detail, repo: &Project, cfg: &BuildConfig, exec_ctx: u64) -> Vec<LintFinding>;
```

### Direction rules

- `goal` is non-empty. Apps have at least 2 milestones and the others have exactly 1.
- Each milestone has at least one `Exit`. A `Claim` must have `why_not_checkable` filled in.
- A bug plan has a `repro_cmd`, and running it **fails** right now. The harness runs it, with the baseline timeout.

### Detail rules

| Rule | Fails when |
|---|---|
| `acceptance.runnable` | an acceptance item has an empty `cmd`, or `cmd` contains "manual", "visually" or "confirm by" |
| `invariant.enforcement` | `enforcement != type` and `why_not_type` is empty |
| `external.simulated` | an external dependency has no simulator description |
| `context_docs.exist` | a context doc path doesn't exist (unless M00 creates it) |
| `files.known` | a `FileIntent` path is outside the repo, or a non-glob path doesn't exist and isn't marked new |
| `risk.security` | the plan touches `secrets`/`auth`/`network`/`unsafe` keywords but no risk has `security: true` |
| `vague` | any free-text field contains a `vague_phrases` entry (whole word, case-insensitive) |

### Task rules

| Rule | Fails when |
|---|---|
| `size.files` | `writes.len() > max_files` |
| `size.ctx` | `est_ctx_tokens > ctx_fraction × executor_ctx` (see below) |
| `writes.exact` | a write path contains glob characters or is outside the repo |
| `writes.unique_owner` | another task in the milestone writes the same path **and** neither depends on the other (scheduling would add an edge, but the splitter should have said so) |
| `symbols.named` | `implement`/`refactor` tasks have no `symbols` |
| `tdd.present` | `tdd.red`/`tdd.green` are missing, or `green` is not in `acceptance` |
| `tdd.test_in_writes` | `tdd.test_path` is not in `writes` (exception: `docs` tasks) |
| `acceptance.runnable` | same as the detail rule |
| `interface.owned` | a task uses an interface key that no task owns and that doesn't exist in the repo yet |
| `simulator.covered` | a task tagged `external:<dep>` doesn't depend, directly or indirectly, on that dependency's simulator task |
| `vague` | `goal`, `out_of_scope` or an acceptance `claim` contains a vague phrase |
| `learned:<id>` | any learned check matches |

**Estimating context.** `est_ctx_tokens = (bytes(reads) + bytes(writes that exist) + bytes(brief text) + bytes(context_docs, capped at context_docs_tokens×4) + tool_schema_bytes) / 4`. `executor_ctx` is `config.ctx` for `loop`. For `cli:*` it is 200k by default, overridable per backend in [10](10-backends-and-prompts.md). Line ranges in `reads` count only those lines.

## Audit loop

```
detail ─► lint ─► (findings? → planner revises with findings) ─► auditor ─► (blockers/majors? → planner revises) ─► pass
             └──────────────── each pass is one AuditRound; max rounds by scale (03) ─────────────────┘
```

The auditor gets the direction, the detail, the lint results (empty at this point), the repo summary and the audit checklist, and answers:

```rust
pub struct AuditAnswer { pub findings: Vec<AuditFindingDraft>, pub pass: bool }
```

`pass` is ignored if any finding is `blocker` or `major`, so the harness makes that decision. When the round limit runs out, the milestone goes to Approval with `verdict: escalated_to_human`, and the approval screen shows the open findings.

### Audit checklist

This is built-in text in `prompts/audit.md`, plus every learned check with `kind: question`:

1. **Gaps:** is every goal and exit covered by acceptance checks? Is anything in scope missing from `files`?
2. **Errors:** does the detail contradict the direction, the repo's code, or itself?
3. **Security:** does the plan handle secrets, untrusted input, auth, network exposure or unsafe code? Is each one a risk with a mitigation?
4. **Testability:** can every acceptance check run offline, quickly and deterministically, against simulators?
5. **Types first:** which invariants enforced by `test` or `assert` could be a type?
6. **Cheap-model fit:** *Could a model that has seen only one task brief, and not this plan, finish each part of this in a single context window without making a design decision?* List every decision that would be left to it.
7. **Granularity:** does anything here look like it would need more than two files changed together?

## Split lint loop

Split ([06](06-split-and-schedule.md)) uses the same pattern: draft → `lint_task` for each task → send the failing tasks back with their findings → repeat, up to the round limit. Tasks that still fail lint become `split_failed` issues instead of blocking the whole milestone.

## Learn

A learned check is a pattern you spotted, saved so it gets caught automatically next time.

```rust
pub struct LearnedCheck {
    pub id: String,                  // "L007"
    pub kind: LearnedKind,           // phrase | regex | question
    pub applies_to: LintTarget,      // direction | detail | task
    pub field: Option<String>,       // e.g. "goal"; None = all text fields
    pub pattern: String,             // phrase/regex, or the audit question
    pub message: String,
    pub from_issue: Option<IssueId>,
}
```

Ways to add one:

- **During triage** ([09](09-gates-and-issues.md#triage)), `(l)earn`: ForgeFlow drafts a check from the issue using the `auditor` role. You edit it and confirm.
- **From the plan menu**, `(l)earn`: type a check by hand.

Each one appends `CheckLearned` to the current plan and writes the check to `build.json`, so it applies to every plan in the repo from then on.

## Tests

- Table tests for each lint rule, passing and failing.
- Estimating context uses line ranges and the context-doc cap.
- The audit loop with a scripted auditor: blockers force another round, `pass: true` with a major finding still fails, and the round limit leads to `escalated_to_human`.
- A learned regex check fires on a task goal and appears in `03_audit.json`.

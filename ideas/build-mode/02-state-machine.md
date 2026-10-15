# 02 · State machine and `forgeflow build`

Modules: `src/build/state.rs`, `src/build/mod.rs`, `src/build/ui.rs`

## Phases

```rust
#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum Phase {
    Intake,
    Direction,
    Milestone { id: MilestoneId, step: MilestoneStep },
    Done,
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MilestoneStep {
    Detail,
    Audit { round: u32 },
    Approval,
    Split { round: u32 },
    Schedule,
    Execute { wave: u32 },
    Checkpoint { wave: u32 },
    Review,
    Replan,          // apps only, after Review when more milestones remain
}
```

## Who owns each step

| Step | Owner | Action | Leaves when |
|---|---|---|---|
| Intake | model, then human | model proposes scale, name and milestone count; human confirms and picks the executor | `IntakeRecorded` |
| Direction | model, then human | model drafts `01_direction` from the source; human edits it in the editor | `DirectionRecorded` |
| Detail | big model (`planner`) | fill in invariants, external dependencies, acceptance checks, risks, context docs | `DetailRecorded` |
| Audit | big model (`auditor`) + lint | lint first, then model audit; problems found send the detail back to the planner | lint clean and the auditor passes it, or 3 rounds and then a human decision |
| Approval | human | read `02` and `03`; approve, or edit and send back to Detail | `Approved` |
| Split | big model (`splitter`) + lint | produce tasks; lint for size and specificity; loop | all tasks pass lint, or 3 rounds and then issues |
| Schedule | deterministic | conflict graph, waves, gates | `Scheduled` |
| Execute | cheap model, escalating to the big one | run ready tasks, verify, run gates | the wave's tasks are all verified or have issues open |
| Checkpoint | human | review the wave's diffs, reports and new issues | `CheckpointCleared` |
| Review | human | run the milestone's exit checks and record the result | `MilestoneReviewed` |
| Replan | big model, then human | update the remaining milestones from what was learned | `Replanned` |

The owner of each model step is a **role**, not a model. [10](10-backends-and-prompts.md) maps roles to backends.

## Reducer and next action

Both are pure functions. They make up most of the test surface.

```rust
pub fn reduce(state: PlanState, event: &Envelope) -> PlanState;

pub enum Action {
    Triage(Vec<IssueId>),                // always first when non-empty
    Human(HumanStep),                    // ui.rs handles
    Model(ModelStep),                    // backend.rs handles
    Deterministic(DetStep),              // schedule, verify-only, render
    Execute { milestone: MilestoneId, wave: u32 },
    Blocked(String),                     // e.g. approval voided, needs human
    Done,
}

pub fn next_action(state: &PlanState) -> Action;
```

`next_action` returns `Triage` whenever there are open issues that block the plan:

- every open issue, when a plan is first selected in a session (the "issues first" rule);
- otherwise, only issues with `pause: true`, such as security findings or escalation failures inside the current wave.

## Driver loop

```rust
pub fn run(mut context: Context) -> Result<(), Box<dyn Error>> {
    let plan = ui::pick_plan(&context.project)?;          // or create
    let mut store = Store::open_or_create(&context.project, plan)?;
    let mut first = true;
    loop {
        let action = state::next_action_with(store.state(), first);
        first = false;
        match action {
            Action::Triage(ids)       => issues::triage(&mut store, &ids)?,
            Action::Human(step)       => ui::human_step(&mut store, step)?,
            Action::Model(step)       => backend::model_step(&mut context, &mut store, step)?,
            Action::Deterministic(s)  => deterministic(&mut store, s)?,
            Action::Execute { .. }    => execute::run_wave(&mut context, &mut store)?,
            Action::Blocked(reason)   => { ui::blocked(&reason); return Ok(()) }
            Action::Done              => { ui::done(&store); return Ok(()) }
        }
    }
}
```

Ctrl-C at any point is safe. Model steps append nothing until their answer has been checked. A task that was running gets `TaskInterrupted` on the next open.

## Terminal UI

```
$ forgeflow build
Plans in /Users/ericolson/dev/forgeflow
  1  261010-143512_GbaDecompiler   app      M01 · execute wave 2/5 · 2 open issues
  2  261011-090210_C25kBeeper      feature  M01 · awaiting approval
  3  261009-101500_ReadmeTweak     tiny     done
  n  new plan
  q  quit
> n
Source: (p)ath to an idea file, (d)escribe it, (b)lank
> p ~/dev/forge/ideas/261010-072929_GbaDecompiler.md
Target repo [/Users/ericolson/dev/forgeflow]: ~/dev/gbadc
  ~/dev/gbadc does not exist. Create it, git init, and set up _forgeflow? [Y/n]
Proposed: app · 5 milestones (M00 Scaffold + Phases 0–3; Phase 4 deferred)
  because: multi-crate system, external tools (mGBA, SWI-Prolog), staged exit criteria
Accept scale? [Y/n/tiny/feature]
Executor for cheap tasks:
  1  loop      ForgeFlow loop · qwen3-coder-30b-a3b (local)
  2  cli:opencode
  3  cli:codex
  4  cli:claude
>
```

Menus are numbered and use one key per choice, built with `crossterm` like the existing setup flow (`src/setup.rs`). Long content (direction, detail, audit, diffs) is never shown inline. The UI prints a one-line summary plus the Markdown path, and offers `(o)pen in editor`.

When the plan is resumed somewhere other than the target repo, `pick_plan` only lists plans in the current repo. Opening another repo's plan means `cd` there first, which keeps `Project::resolve` confinement simple.

### Approval screen

```
M01 SkeletonAndScoring · ready for approval
  detail   milestones/M01_SkeletonAndScoring/02_detail.md   (7 invariants, 2 simulators, 11 checks)
  audit    milestones/M01_SkeletonAndScoring/03_audit.md    (3 rounds · lint clean · 0 open findings)
(a)pprove · (o)pen detail · (e)dit and send back · (q)uit
```

Approval stores the sha256 of `01_direction.json` and `02_detail.json` ([04](04-phase-schemas.md#approval)). On every `open`, the hashes are recomputed. If they no longer match, `ApprovalVoided` is appended and the milestone returns to Approval.

### Checkpoint screen

```
M01 · wave 2 complete · 5 verified · 1 escalated (T019) · 1 new issue
  T014  add Provider::Anthropic          +42 −3   report ✓
  T015  …
(d)iff a task · (r)eport · (i)ssues · (c)ontinue · (q)uit
```

## Tests

- A reducer table: (state, event) → state for every event.
- `next_action` for every phase and step, including the issues-first rule and approval voiding.
- A driver test with a scripted backend and scripted UI: a tiny plan runs from intake to done in memory, with Ctrl-C injected between every pair of events, and the plan resumes each time.

# 10 · Backends, roles and prompts

Modules: `src/build/backend.rs`; changes in `src/agent.rs`

## Backend specs

```rust
pub enum BackendSpec {
    Loop { model: Option<String> },                 // ForgeFlow loop; None = config.model / endpoint
    Cli { agent: Provider, model: Option<String> }, // claude | codex | opencode
}
```

The text form is used in config and the UI, and is parsed with `FromStr` and printed with `Display`:

| Text | Meaning |
|---|---|
| `loop` | ForgeFlow loop with the selected local model or endpoint |
| `loop:qwen3-coder-30b-a3b` | ForgeFlow loop with a specific catalog model |
| `cli:claude` | Claude Code CLI, default model |
| `cli:claude:sonnet` | with `--model sonnet` |
| `cli:codex:gpt-5-mini` | with `-m gpt-5-mini` |
| `cli:opencode:openrouter/qwen/qwen3-coder` | with `-m openrouter/qwen/qwen3-coder` |

The plan's **executor** is a `BackendSpec` chosen at intake ([03](03-scale-and-milestones.md#intake)). Every other model step uses a **role**.

## Roles

`_forgeflow/config.json` is per-machine and not tracked in git:

```json
"build": {
  "roles": {
    "planner":    "cli:claude",
    "auditor":    "cli:claude",
    "splitter":   "cli:claude",
    "reviewer":   "cli:codex",
    "security":   "cli:claude",
    "escalation": "cli:claude"
  },
  "cli": {
    "claude":   { "ctx": 200000, "timeout_s": 1800, "read_args": [], "exec_args": [] },
    "codex":    { "ctx": 200000, "timeout_s": 1800, "read_args": [], "exec_args": [] },
    "opencode": { "ctx": 128000, "timeout_s": 1800, "read_args": [], "exec_args": [] }
  }
}
```

- A role that isn't set falls back to the first of `claude`, `codex`, `opencode` found on `PATH`. If none is found, the UI asks for one the first time it's needed.
- Roles can also be `loop` (for example a strong local model as `reviewer`).
- **`read_args`** are used for planning roles, which must not edit files. **`exec_args`** are used for execution and escalation, which must be able to edit and run commands.
  - Built-in defaults are chosen per agent: Claude Code `--permission-mode plan` for reading and `--permission-mode acceptEdits` plus allowed Bash for executing; Codex `--sandbox read-only` and `--full-auto`.
  - Check these flags against the installed CLI versions while implementing B1. They live in config so you can change them without a rebuild.
  - Running an execution agent with broad permissions in your repo is a real trust decision. The intake screen shows the exact `exec_args` before you confirm.

## `Agent` changes

`Agent::ask` doesn't take a working directory, model, timeout or extra arguments, and doesn't return usage. It gains:

```rust
pub struct AskOptions<'a> {
    pub cwd: &'a Path,
    pub model: Option<&'a str>,
    pub timeout: Duration,
    pub extra_args: &'a [String],
}
pub struct AskResult { pub text: String, pub usage: Option<Usage> }

impl Agent {
    pub fn ask_with(&self, prompt: &str, options: &AskOptions) -> io::Result<AskResult>;
}
```

- **Model flag:** Claude `--model`, Codex `-m`, opencode `-m`.
- **Timeout:** a watcher thread kills the child when the deadline passes, and the result is `ErrorKind::TimedOut`.
- **Usage:** read from each CLI's final JSON event when it's there (Claude's `result` event has `usage` and `total_cost_usd`). Otherwise `None`.
- **Long prompts:** prompts over about 100 KB go through stdin rather than argv, so they don't hit `ARG_MAX`.

`ask` stays as a thin wrapper so existing callers don't change.

## Structured answers

```rust
pub fn ask_json<T>(context: &mut Context, spec: &BackendSpec, prompt: &Prompt, validate: impl Fn(&T) -> Vec<String>)
    -> Result<(T, Vec<Usage>), AskError>
where T: DeserializeOwned + JsonSchema;
```

1. Render the prompt and append: *"Answer with only a JSON object matching this schema:"* plus `schemars::schema_for!(T)`.
2. Call the backend:
   - **`loop`:** a single `ChatBackend::step` with no tools and llama-server's `json_schema` constraint. The output always parses, so only the semantic `validate` can fail.
   - **`cli:*`:** `Agent::ask_with` with `read_args`.
3. Extract the JSON: a ```` ```json ```` fence first, otherwise the outermost `{…}`.
4. Run `serde_json::from_str::<T>`, then `validate`.
5. On any error, ask again with the previous answer and a numbered list of errors. Allow **2 corrections**, then return `AskError` carrying all attempts.

A model step that ends in `AskError` doesn't append its phase event. It opens a blocking `human` issue with the attempts attached. The plan stays where it was, and you can retry or change the role's backend.

## Prompts

These are built into `src/build/prompts/*.md` with `include_str!`. A project can override any of them with `_forgeflow/prompts/<name>.md`, so you can tune prompts without rebuilding.

| Name | Role | Used by |
|---|---|---|
| `intake` | planner | scale proposal ([03](03-scale-and-milestones.md)) |
| `direction` | planner | direction draft, replan |
| `detail` | planner | detail, and revisions from audit findings |
| `audit` | auditor | audit round + checklist ([05](05-audit-and-lint.md#audit-checklist)) |
| `split` | splitter | split, re-split, refactor gates, promote ([06](06-split-and-schedule.md)) |
| `review-general`, `review-security` | reviewer, security | review gates ([09](09-gates-and-issues.md)) |
| `learn` | auditor | drafting a learned check from an issue |
| `executor` | — | system prompt for `loop` tasks ([08](08-loop-write-tools.md)) |
| `escalation` | escalation | preamble added before the brief on attempt 3 |

Templates fill in `{{name}}` placeholders from a `BTreeMap<&str, String>`. An unknown placeholder is an error, caught by a test that renders every prompt.

## Usage and comparing executors

Every model call records a `Usage` in its event: input/output tokens, `cost_usd` when the CLI reports it, and wall time. The plan menu's `(s)tats` view adds these up for each role and each executor tier:

```
GbaDecompiler · M01
  executor loop (qwen3-coder-30b-a3b)  31 tasks · first-try 71% · escalated 4 · issues 1 · 2h14m · $0.00
  escalation cli:claude                 4 runs · 3 verified · $1.84
  planner/auditor/splitter cli:claude  $6.12
```

This is how you compare `loop` and `cli:*` executors. Run the same tiny or feature plan once with each (create it twice at intake) and compare the stats.

## Tests

- `BackendSpec` text round-trips, and bad specs give useful errors.
- `ask_with` arguments for each agent, model and mode (`read`/`exec`), checked with a fake binary on `PATH`.
- Timeout kills the child.
- `ask_json`: a fenced answer, a bare answer, a schema error leading to a correction, a validation error leading to a correction, and attempts running out.
- Every built-in prompt renders with the variables its caller provides.

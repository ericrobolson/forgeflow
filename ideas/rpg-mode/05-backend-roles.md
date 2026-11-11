# 05 · Backends and roles

Module: `src/rpg/backend.rs`

## Roles, not models

As in build mode, the narrator, archivist, and compel-decider are **roles** mapped to backends in the session's `meta.json`; code never names a provider. A role is filled by a backend that implements the right trait.

```json
{
  "roles": {
    "narrator":       "local:qwen2.5-32b",
    "archivist":      "local:qwen2.5-7b",
    "compel_decider": "rules"
  }
}
```

Swap `"compel_decider"` to `"jev"` or `"local:qwen2.5-0.5b"` — nothing else changes.

## Two traits, by what they return

```rust
/// Produces text. The narrator. The existing llama-server agent loop.
pub trait Generation {
    fn narrate(&mut self, ctx: &SceneContext, compels: &[CompelFlag], action: &str,
               store: &mut Store, sink: &mut dyn Sink) -> Result<Narration, GenError>;
}

/// Produces typed structured answers, never free text. The black box.
pub trait Decision {
    fn decide(&mut self, query: &DecisionQuery) -> Result<DecisionAnswer, DecisionError>;
}

pub enum DecisionQuery {
    Compel  { aspect_id: i64, scene_id: i64, action_text: String },
    InScope { aspect_id: i64, scene_id: i64 },
    Salient { candidate: String, scene_id: i64 },
    Tone    { scene_id: i64 },
}

pub struct DecisionAnswer { pub yes: bool, pub confidence: f32, pub reason: Option<String> }
```

A local LLM can implement **both**; Jev implements only `Decision`.

## The Decision black box: three implementations, one trait

The interface is **typed decisions**. Whether the backend is natively typed or text-constrained-to-typed is the backend's problem, hidden behind the trait.

| Backend | `decide` implementation | Notes |
|---|---|---|
| `RulesBackend` | deterministic heuristics (compel if a `trouble`-slot aspect + tense-scene keywords; salience by entity-name match) | v1 default — zero latency, free, offline, no model |
| `LocalLlmBackend` | tiny local model (Qwen-0.5B / Phi-4-mini) **grammar-constrained** (llama-server GBNF / JSON schema) to emit `DecisionAnswer`; parse + validate + one retry on bad JSON | local, private, offline |
| `JevBackend` | calls Jev with the query as a noul; native typed probabilistic answer | cloud; fastest/cheapest at scale |

Every answer is parsed into `DecisionAnswer` and validated before use. Bad JSON gets one retry with the error attached, then is dropped (the build-mode discipline). A dropped compel decision is treated as `no` — the turn proceeds.

## One abstraction, several call sites

The `Decision` trait is not only for compels. The same backend fills:

| Call site | Query | Used by |
|---|---|---|
| compel pass | `Compel` | turn loop, step 2 |
| relevance routing | `InScope` | (later) narrowing which aspects to consider |
| fact salience | `Salient` | archivist, step 4 |
| scene tone/mood | `Tone` | `scene.rs` image-prompt assembler |

So a single configured backend serves every classification need in the mode.

## Generation backends

| Backend | `narrate` implementation |
|---|---|
| `LocalLlmBackend` | the existing agent loop (`src/agent_loop.rs`): llama-server for the selected model, world words as tools, results in scratch registers, short receipts |
| (later) `CliBackend` | a CLI agent behind the loop, as in build mode's `cli:<agent>` executor |

The narrator's prompt files live in `src/rpg/prompts/*.md` (narrator, archivist, compel), overridable per project — the same pattern as build mode's `prompts/`.

## Roles struct

```rust
pub struct Roles {
    pub narrator:       Box<dyn Generation>,
    pub archivist:      Box<dyn Generation>,   // uses a smaller model, same trait
    pub compel_decider: Box<dyn Decision>,
}

impl Roles {
    pub fn load(meta: &Meta, project: &Project) -> Result<Self, String>;  // resolves role→backend
}
```

`archivist` is `Generation` (it writes facts as structured tool calls, not free decisions), running on a cheaper model. `compel_decider` is `Decision`.

## v1 defaults

- `narrator` → the project's selected model (the one `forgeflow use` set).
- `archivist` → the same model (v1; split later).
- `compel_decider` → `RulesBackend` (no extra model; the narrator handles any compels the rules miss by reading flagged aspects). The `JevBackend` and `LocalLlmBackend` impls exist and are selectable, but are opt-in.

## Tests

- `RulesBackend` compel: returns `yes` for a trouble-slot aspect in a tense scene, `no` otherwise — table-driven.
- `LocalLlmBackend`: a stub llama-server returning valid `DecisionAnswer` JSON is parsed; invalid JSON triggers one retry then `no`.
- `JevBackend`: a stub HTTP Jev returning a noul answer maps to `DecisionAnswer`.
- `Roles::load` resolves each role from `meta.json`; an unknown backend string is a located error.
- A turn with `compel_decider = "rules"` runs end-to-end with no second model started.

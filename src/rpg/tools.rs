//! Typed world tool schemas, validation, and register receipts.
use super::{
    scenes::{Participant, SceneInput, get_scene_context},
    world::{AspectInput, Scope, World},
};
use crate::{tools::ToolOutcome, value::Value, workflow::Context};
use serde_json::{Value as Json, json};
/// The accepted JSON type of a world-tool argument.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParamKind {
    Int,
    Text,
    IntList,
    TextList,
    Participants,
    Json,
}
/// One named world-tool argument and its validation metadata.
pub struct Param {
    pub name: &'static str,
    pub kind: ParamKind,
    pub required: bool,
    pub description: &'static str,
}
/// Whether a tool reads the world or changes it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Effect {
    Query,
    Mutate,
}
/// A world operation exposed to the narrator.
pub struct WorldTool {
    pub name: &'static str,
    pub effect: Effect,
    pub description: &'static str,
    pub params: &'static [Param],
}
/// All supported world operations and their typed parameters.
pub const TOOLS: &[WorldTool] = &[
    WorldTool {
        name: "scene",
        effect: Effect::Query,
        description: "Read the active scene, present entities, world facts and recent events.",
        params: &[],
    },
    WorldTool {
        name: "aspects",
        effect: Effect::Query,
        description: "List current aspects for a holder, or campaign aspects when holder_id is absent.",
        params: &[Param {
            name: "holder_id",
            kind: ParamKind::Int,
            required: false,
            description: "Existing world row id.",
        }],
    },
    WorldTool {
        name: "aspects-at",
        effect: Effect::Query,
        description: "List aspects true at a specified in-world time.",
        params: &[
            Param {
                name: "holder_id",
                kind: ParamKind::Int,
                required: false,
                description: "Existing world row id.",
            },
            Param {
                name: "game_time",
                kind: ParamKind::Text,
                required: true,
                description: "In-world time as YYYY-MM-DDTHH:MM.",
            },
        ],
    },
    WorldTool {
        name: "entity",
        effect: Effect::Query,
        description: "Read an entity and its current aspects, skills, stress, behaviors and fate points.",
        params: &[Param {
            name: "id",
            kind: ParamKind::Int,
            required: true,
            description: "Existing world row id.",
        }],
    },
    WorldTool {
        name: "relationships",
        effect: Effect::Query,
        description: "List current outgoing relationships, optionally restricted to one target.",
        params: &[
            Param {
                name: "from_id",
                kind: ParamKind::Int,
                required: true,
                description: "Existing world row id.",
            },
            Param {
                name: "to_id",
                kind: ParamKind::Int,
                required: false,
                description: "Existing world row id.",
            },
        ],
    },
    WorldTool {
        name: "behaviors",
        effect: Effect::Query,
        description: "List the current stunts and dispositions of an entity.",
        params: &[Param {
            name: "entity_id",
            kind: ParamKind::Int,
            required: true,
            description: "Existing world row id.",
        }],
    },
    WorldTool {
        name: "skills",
        effect: Effect::Query,
        description: "List current skills and approaches with Fate ladder ranks.",
        params: &[Param {
            name: "entity_id",
            kind: ParamKind::Int,
            required: true,
            description: "Existing world row id.",
        }],
    },
    WorldTool {
        name: "stress",
        effect: Effect::Query,
        description: "List current stress tracks with marked and total box counts.",
        params: &[Param {
            name: "entity_id",
            kind: ParamKind::Int,
            required: true,
            description: "Existing world row id.",
        }],
    },
    WorldTool {
        name: "search-aspects",
        effect: Effect::Query,
        description: "Search current aspects by full-text keywords.",
        params: &[Param {
            name: "keywords",
            kind: ParamKind::Text,
            required: true,
            description: "Words to search; punctuation is ignored and matching keywords are combined with OR.",
        }],
    },
    WorldTool {
        name: "log-event",
        effect: Effect::Mutate,
        description: "Record a meaningful moment and advance game time only forward.",
        params: &[
            Param {
                name: "game_time",
                kind: ParamKind::Text,
                required: true,
                description: "In-world time as YYYY-MM-DDTHH:MM.",
            },
            Param {
                name: "summary",
                kind: ParamKind::Text,
                required: true,
                description: "Event summary, 1 to 300 characters.",
            },
            Param {
                name: "actor_id",
                kind: ParamKind::Int,
                required: false,
                description: "Existing world row id.",
            },
        ],
    },
    WorldTool {
        name: "add-entity",
        effect: Effect::Mutate,
        description: "Add a named entity, including characters, locations, items or hazards.",
        params: &[
            Param {
                name: "name",
                kind: ParamKind::Text,
                required: true,
                description: "Entity name, 1 to 80 characters.",
            },
            Param {
                name: "type",
                kind: ParamKind::Text,
                required: true,
                description: "Entity type, 1 to 40 lowercase letters, digits, underscores or hyphens.",
            },
            Param {
                name: "attributes",
                kind: ParamKind::Json,
                required: false,
                description: "JSON object of entity attributes.",
            },
        ],
    },
    WorldTool {
        name: "add-aspect",
        effect: Effect::Mutate,
        description: "Add a short world truth, replacing a current aspect only when its slot matches.",
        params: &[
            Param {
                name: "text",
                kind: ParamKind::Text,
                required: true,
                description: "Short aspect phrase, 1 to 80 characters.",
            },
            Param {
                name: "scope",
                kind: ParamKind::Text,
                required: true,
                description: "Aspect scope: character, situation, campaign, item or consequence.",
            },
            Param {
                name: "holder_id",
                kind: ParamKind::Int,
                required: false,
                description: "Existing world row id.",
            },
            Param {
                name: "slot",
                kind: ParamKind::Text,
                required: false,
                description: "Optional aspect slot, such as high-concept, trouble or consequence-mild.",
            },
            Param {
                name: "valid_from",
                kind: ParamKind::Text,
                required: false,
                description: "Start time as YYYY-MM-DDTHH:MM, at or after the current clock.",
            },
        ],
    },
    WorldTool {
        name: "rename-aspect",
        effect: Effect::Mutate,
        description: "Insert a renamed successor while preserving the aspect history.",
        params: &[
            Param {
                name: "id",
                kind: ParamKind::Int,
                required: true,
                description: "Existing world row id.",
            },
            Param {
                name: "new_text",
                kind: ParamKind::Text,
                required: true,
                description: "Replacement aspect phrase, 1 to 80 characters.",
            },
        ],
    },
    WorldTool {
        name: "expire-aspect",
        effect: Effect::Mutate,
        description: "Close an aspect at a game time without inserting a successor.",
        params: &[
            Param {
                name: "id",
                kind: ParamKind::Int,
                required: true,
                description: "Existing world row id.",
            },
            Param {
                name: "valid_to",
                kind: ParamKind::Text,
                required: false,
                description: "End time as YYYY-MM-DDTHH:MM, at or after the row begins.",
            },
            Param {
                name: "ended_event_id",
                kind: ParamKind::Int,
                required: false,
                description: "Existing world row id.",
            },
        ],
    },
    WorldTool {
        name: "update-relationship",
        effect: Effect::Mutate,
        description: "Insert a relationship version with a score from -100 to 100.",
        params: &[
            Param {
                name: "from",
                kind: ParamKind::Int,
                required: true,
                description: "Existing world row id.",
            },
            Param {
                name: "to",
                kind: ParamKind::Int,
                required: true,
                description: "Existing world row id.",
            },
            Param {
                name: "type",
                kind: ParamKind::Text,
                required: true,
                description: "Relationship type, 1 to 40 characters.",
            },
            Param {
                name: "score",
                kind: ParamKind::Int,
                required: true,
                description: "Relationship score: -100 (hatred) to 100 (devotion).",
            },
            Param {
                name: "valid_from",
                kind: ParamKind::Text,
                required: false,
                description: "Start time as YYYY-MM-DDTHH:MM, at or after the current clock.",
            },
        ],
    },
    WorldTool {
        name: "set-skill",
        effect: Effect::Mutate,
        description: "Set a skill or approach to a Fate ladder rank from 0 to 8.",
        params: &[
            Param {
                name: "entity_id",
                kind: ParamKind::Int,
                required: true,
                description: "Existing world row id.",
            },
            Param {
                name: "name",
                kind: ParamKind::Text,
                required: true,
                description: "Track or skill name, 1 to 40 characters.",
            },
            Param {
                name: "rank",
                kind: ParamKind::Int,
                required: true,
                description: "Fate ladder rank from 0 to 8.",
            },
            Param {
                name: "kind",
                kind: ParamKind::Text,
                required: false,
                description: "Skill kind: skill (default) or approach.",
            },
            Param {
                name: "valid_from",
                kind: ParamKind::Text,
                required: false,
                description: "Start time as YYYY-MM-DDTHH:MM, at or after the current clock.",
            },
        ],
    },
    WorldTool {
        name: "add-behavior",
        effect: Effect::Mutate,
        description: "Add a stunt or disposition triggered by a specific condition.",
        params: &[
            Param {
                name: "entity_id",
                kind: ParamKind::Int,
                required: true,
                description: "Existing world row id.",
            },
            Param {
                name: "kind",
                kind: ParamKind::Text,
                required: true,
                description: "Behavior kind: stunt or disposition.",
            },
            Param {
                name: "trigger",
                kind: ParamKind::Text,
                required: true,
                description: "Behavior trigger, 1 to 120 characters.",
            },
            Param {
                name: "action",
                kind: ParamKind::Text,
                required: true,
                description: "Behavior action, 1 to 120 characters.",
            },
            Param {
                name: "priority",
                kind: ParamKind::Int,
                required: false,
                description: "Behavior priority as a signed integer, default 0.",
            },
        ],
    },
    WorldTool {
        name: "add-stress-track",
        effect: Effect::Mutate,
        description: "Add a uniquely named stress track with 1 to 10 boxes.",
        params: &[
            Param {
                name: "entity_id",
                kind: ParamKind::Int,
                required: true,
                description: "Existing world row id.",
            },
            Param {
                name: "name",
                kind: ParamKind::Text,
                required: true,
                description: "Track or skill name, 1 to 40 characters.",
            },
            Param {
                name: "size",
                kind: ParamKind::Int,
                required: true,
                description: "Stress track capacity from 1 to 10 boxes.",
            },
        ],
    },
    WorldTool {
        name: "mark-stress",
        effect: Effect::Mutate,
        description: "Mark a positive number of boxes without overflowing a stress track.",
        params: &[
            Param {
                name: "entity_id",
                kind: ParamKind::Int,
                required: true,
                description: "Existing world row id.",
            },
            Param {
                name: "track",
                kind: ParamKind::Text,
                required: true,
                description: "Name of a current stress track, matched case-insensitively.",
            },
            Param {
                name: "delta",
                kind: ParamKind::Int,
                required: true,
                description: "Positive number of stress boxes.",
            },
        ],
    },
    WorldTool {
        name: "clear-stress",
        effect: Effect::Mutate,
        description: "Clear a positive number of boxes, stopping at zero marked boxes.",
        params: &[
            Param {
                name: "entity_id",
                kind: ParamKind::Int,
                required: true,
                description: "Existing world row id.",
            },
            Param {
                name: "track",
                kind: ParamKind::Text,
                required: true,
                description: "Name of a current stress track, matched case-insensitively.",
            },
            Param {
                name: "delta",
                kind: ParamKind::Int,
                required: true,
                description: "Positive number of stress boxes.",
            },
        ],
    },
    WorldTool {
        name: "join-scene",
        effect: Effect::Mutate,
        description: "Join an open scene as pc, npc, prop or observer.",
        params: &[
            Param {
                name: "entity_id",
                kind: ParamKind::Int,
                required: true,
                description: "Existing world row id.",
            },
            Param {
                name: "scene_id",
                kind: ParamKind::Int,
                required: true,
                description: "Existing world row id.",
            },
            Param {
                name: "role",
                kind: ParamKind::Text,
                required: false,
                description: "Presence role: pc, npc, prop or observer.",
            },
        ],
    },
    WorldTool {
        name: "leave-scene",
        effect: Effect::Mutate,
        description: "Close an entity presence in this scene only.",
        params: &[
            Param {
                name: "entity_id",
                kind: ParamKind::Int,
                required: true,
                description: "Existing world row id.",
            },
            Param {
                name: "scene_id",
                kind: ParamKind::Int,
                required: true,
                description: "Existing world row id.",
            },
        ],
    },
    WorldTool {
        name: "begin-scene",
        effect: Effect::Mutate,
        description: "Begin a scene with a location, chapter, participants and situation aspects.",
        params: &[
            Param {
                name: "title",
                kind: ParamKind::Text,
                required: true,
                description: "Scene title, 1 to 80 characters.",
            },
            Param {
                name: "summary",
                kind: ParamKind::Text,
                required: false,
                description: "Optional scene prose.",
            },
            Param {
                name: "location_id",
                kind: ParamKind::Int,
                required: false,
                description: "Existing world row id.",
            },
            Param {
                name: "chapter",
                kind: ParamKind::Int,
                required: false,
                description: "Chapter number, at least 1.",
            },
            Param {
                name: "participants",
                kind: ParamKind::Participants,
                required: false,
                description: "Scene participants with entity ids and pc, npc, prop or observer roles.",
            },
            Param {
                name: "situation_aspects",
                kind: ParamKind::TextList,
                required: false,
                description: "List of short aspect phrases.",
            },
        ],
    },
    WorldTool {
        name: "end-scene",
        effect: Effect::Mutate,
        description: "End a scene, carrying selected situation aspects to its location and expiring the rest.",
        params: &[
            Param {
                name: "scene_id",
                kind: ParamKind::Int,
                required: true,
                description: "Existing world row id.",
            },
            Param {
                name: "carry_over",
                kind: ParamKind::IntList,
                required: false,
                description: "Current situation aspect ids to carry to the scene location.",
            },
        ],
    },
    WorldTool {
        name: "cut-to-scene",
        effect: Effect::Mutate,
        description: "Select an existing scene, loading closed scenes as flashbacks.",
        params: &[Param {
            name: "scene_id",
            kind: ParamKind::Int,
            required: true,
            description: "Existing world row id.",
        }],
    },
    WorldTool {
        name: "spend-fate",
        effect: Effect::Mutate,
        description: "Spend a positive number of available character fate points.",
        params: &[
            Param {
                name: "entity_id",
                kind: ParamKind::Int,
                required: true,
                description: "Existing world row id.",
            },
            Param {
                name: "amount",
                kind: ParamKind::Int,
                required: true,
                description: "Positive number of fate points.",
            },
            Param {
                name: "reason",
                kind: ParamKind::Text,
                required: true,
                description: "Non-empty explanation for the fate-point transaction.",
            },
            Param {
                name: "aspect_id",
                kind: ParamKind::Int,
                required: false,
                description: "Existing world row id.",
            },
        ],
    },
    WorldTool {
        name: "award-fate",
        effect: Effect::Mutate,
        description: "Award a positive number of fate points to a character.",
        params: &[
            Param {
                name: "entity_id",
                kind: ParamKind::Int,
                required: true,
                description: "Existing world row id.",
            },
            Param {
                name: "amount",
                kind: ParamKind::Int,
                required: true,
                description: "Positive number of fate points.",
            },
            Param {
                name: "reason",
                kind: ParamKind::Text,
                required: true,
                description: "Non-empty explanation for the fate-point transaction.",
            },
            Param {
                name: "aspect_id",
                kind: ParamKind::Int,
                required: false,
                description: "Existing world row id.",
            },
        ],
    },
];
/// Builds world schemas followed by register queries and project words.
pub fn schemas(words: &[crate::user_words::UserWord]) -> Vec<Json> {
    let mut out=TOOLS.iter().map(|t|{
        let mut props=serde_json::Map::new();let mut required=vec![];
        for p in t.params {let mut s=match p.kind {ParamKind::Int=>json!({"type":"integer"}),ParamKind::Text=>json!({"type":"string"}),ParamKind::IntList=>json!({"type":"array","items":{"type":"integer"}}),ParamKind::TextList=>json!({"type":"array","items":{"type":"string"}}),ParamKind::Json=>json!({"type":"object"}),ParamKind::Participants=>json!({"type":"array","items":{"type":"object","properties":{"entity_id":{"type":"integer"},"role":{"type":"string","enum":["pc","npc","prop","observer"]}},"required":["entity_id","role"]}})};s["description"]=p.description.into();props.insert(p.name.into(),s);if p.required{required.push(p.name);}}
        json!({"type":"function","function":{"name":t.name,"description":t.description,"parameters":{"type":"object","properties":props,"required":required}}})
    }).collect::<Vec<_>>();
    let base = crate::tools::schemas();
    out.extend(crate::tools::schemas_with(words).into_iter().filter(|s| {
        let n = &s["function"]["name"];
        n == "reg_view" || n == "reg_search" || !base.iter().any(|b| b["function"]["name"] == *n)
    }));
    out
}
fn arguments(t: &WorldTool, raw: &str) -> Result<Json, String> {
    let mut value: Json = serde_json::from_str(if raw.trim().is_empty() { "{}" } else { raw })
        .map_err(|e| format!("arguments are not valid JSON: {e}"))?;
    let object = value
        .as_object_mut()
        .ok_or("arguments must be a JSON object")?;
    for key in object.keys() {
        if !t.params.iter().any(|p| p.name == key) {
            return Err(format!("unexpected argument `{key}`"));
        }
    }
    for p in t.params {
        if object.get(p.name).is_none_or(Json::is_null) {
            if p.required {
                return Err(format!("missing argument `{}`", p.name));
            }
            object.remove(p.name);
            continue;
        }
        let v = object.get_mut(p.name).ok_or("missing argument")?;
        if p.kind == ParamKind::Int {
            if let Some(n) = v.as_str().and_then(|s| s.parse::<i64>().ok()) {
                *v = n.into();
            }
        }
        let (valid, expected) = match p.kind {
            ParamKind::Int => (v.as_i64().is_some(), "an integer"),
            ParamKind::Text => (v.is_string(), "a string"),
            ParamKind::Json => (v.is_object(), "a JSON object"),
            ParamKind::IntList => (
                v.as_array()
                    .is_some_and(|a| a.iter().all(|v| v.as_i64().is_some())),
                "a list of integers",
            ),
            ParamKind::TextList => (
                v.as_array().is_some_and(|a| a.iter().all(Json::is_string)),
                "a list of strings",
            ),
            ParamKind::Participants => (
                serde_json::from_value::<Vec<Participant>>(v.clone()).is_ok(),
                "a list of participant objects",
            ),
        };
        if !valid {
            return Err(format!("argument `{}`: expected {expected}", p.name));
        }
    }
    Ok(value)
}
/// Executes a world tool or an allowed register query or project word.
pub fn execute(context: &mut Context, world: &mut World, name: &str, raw: &str) -> ToolOutcome {
    let Some(tool) = TOOLS.iter().find(|t| t.name == name) else {
        return if ["reg_view", "reg_search"].contains(&name)
            || context.user_words.iter().any(|w| w.name == name)
        {
            crate::tools::execute(context, name, raw)
        } else {
            ToolOutcome {
                content: format!("error: unknown tool `{name}`"),
                is_error: true,
            }
        };
    };
    let result: Result<String, String> = (|| {
        let value = arguments(tool, raw)?;
        let text = dispatch(world, name, value)?;
        if tool.effect == Effect::Query {
            let a = context.registers.allocate(
                Value::String(text),
                &format!("{name} {}", raw.chars().take(40).collect::<String>()),
            );
            let mut receipt = format!("→ {}", context.registers.receipt(a.register));
            if let Some(replaced) = a.replaced {
                receipt += &format!(" (replaced: {replaced})");
            }
            Ok(receipt)
        } else {
            Ok(text)
        }
    })();
    match result {
        Ok(content) => ToolOutcome {
            content,
            is_error: false,
        },
        Err(e) => ToolOutcome {
            content: format!("error: {e}"),
            is_error: true,
        },
    }
}
fn pretty(value: impl serde::Serialize) -> Result<String, String> {
    serde_json::to_string_pretty(&value).map_err(|e| e.to_string())
}
fn dispatch(w: &mut World, name: &str, value: Json) -> Result<String, String> {
    match name {
        "scene" => {
            return Ok(get_scene_context(w.db, w.meta, w.meta.current_scene_id, vec![])?.render());
        }
        "aspects" => {
            #[derive(serde::Deserialize)]
            struct Args {
                holder_id: Option<i64>,
            }
            let a: Args = serde_json::from_value(value).map_err(|e| e.to_string())?;
            return pretty(w.aspects(a.holder_id)?);
        }
        "aspects-at" => {
            #[derive(serde::Deserialize)]
            struct Args {
                holder_id: Option<i64>,
                game_time: String,
            }
            let a: Args = serde_json::from_value(value).map_err(|e| e.to_string())?;
            return pretty(w.aspects_at(a.holder_id, &a.game_time)?);
        }
        "entity" => {
            #[derive(serde::Deserialize)]
            struct Args {
                id: i64,
            }
            let a: Args = serde_json::from_value(value).map_err(|e| e.to_string())?;
            return pretty(w.entity_record(a.id)?);
        }
        "relationships" => {
            #[derive(serde::Deserialize)]
            struct Args {
                from_id: i64,
                to_id: Option<i64>,
            }
            let a: Args = serde_json::from_value(value).map_err(|e| e.to_string())?;
            return pretty(w.relationships(a.from_id, a.to_id)?);
        }
        "behaviors" => {
            #[derive(serde::Deserialize)]
            struct Args {
                entity_id: i64,
            }
            let a: Args = serde_json::from_value(value).map_err(|e| e.to_string())?;
            return pretty(w.behaviors(a.entity_id)?);
        }
        "skills" => {
            #[derive(serde::Deserialize)]
            struct Args {
                entity_id: i64,
            }
            let a: Args = serde_json::from_value(value).map_err(|e| e.to_string())?;
            return pretty(w.skills(a.entity_id)?);
        }
        "stress" => {
            #[derive(serde::Deserialize)]
            struct Args {
                entity_id: i64,
            }
            let a: Args = serde_json::from_value(value).map_err(|e| e.to_string())?;
            return pretty(w.stress(a.entity_id)?);
        }
        "search-aspects" => {
            #[derive(serde::Deserialize)]
            struct Args {
                keywords: String,
            }
            let a: Args = serde_json::from_value(value).map_err(|e| e.to_string())?;
            return pretty(w.search_aspects(&a.keywords)?);
        }
        "log-event" => {
            #[derive(serde::Deserialize)]
            struct Args {
                game_time: String,
                summary: String,
                actor_id: Option<i64>,
            }
            let a: Args = serde_json::from_value(value).map_err(|e| e.to_string())?;
            let id = w.log_event(&a.game_time, &a.summary, a.actor_id)?;
            Ok(format!("event #{id} logged"))
        }
        "add-entity" => {
            #[derive(serde::Deserialize)]
            struct Args {
                name: String,
                #[serde(rename = "type")]
                kind: String,
                attributes: Option<Json>,
            }
            let a: Args = serde_json::from_value(value).map_err(|e| e.to_string())?;
            let id = w.add_entity(&a.name, &a.kind, a.attributes)?;
            Ok(format!("entity #{id} added"))
        }
        "add-aspect" => {
            #[derive(serde::Deserialize)]
            struct Args {
                text: String,
                scope: String,
                holder_id: Option<i64>,
                slot: Option<String>,
                valid_from: Option<String>,
            }
            let a: Args = serde_json::from_value(value).map_err(|e| e.to_string())?;
            let id = w.add_aspect(AspectInput {
                text: a.text,
                scope: Scope::parse(&a.scope)?,
                holder_id: a.holder_id,
                slot: a.slot,
                valid_from: a.valid_from,
            })?;
            Ok(format!("aspect #{id} added"))
        }
        "rename-aspect" => {
            #[derive(serde::Deserialize)]
            struct Args {
                id: i64,
                new_text: String,
            }
            let a: Args = serde_json::from_value(value).map_err(|e| e.to_string())?;
            let new = w.rename_aspect(a.id, &a.new_text)?;
            Ok(format!("aspect #{} renamed to #{new}", a.id))
        }
        "expire-aspect" => {
            #[derive(serde::Deserialize)]
            struct Args {
                id: i64,
                valid_to: Option<String>,
                ended_event_id: Option<i64>,
            }
            let a: Args = serde_json::from_value(value).map_err(|e| e.to_string())?;
            w.expire_aspect(a.id, a.valid_to.as_deref(), a.ended_event_id)?;
            Ok(format!("aspect #{} expired", a.id))
        }
        "update-relationship" => {
            #[derive(serde::Deserialize)]
            struct Args {
                from: i64,
                to: i64,
                #[serde(rename = "type")]
                kind: String,
                score: i64,
                valid_from: Option<String>,
            }
            let a: Args = serde_json::from_value(value).map_err(|e| e.to_string())?;
            let id =
                w.update_relationship(a.from, a.to, &a.kind, a.score, a.valid_from.as_deref())?;
            Ok(format!("relationship #{id} set"))
        }
        "set-skill" => {
            #[derive(serde::Deserialize)]
            struct Args {
                entity_id: i64,
                name: String,
                rank: i64,
                kind: Option<String>,
                valid_from: Option<String>,
            }
            let a: Args = serde_json::from_value(value).map_err(|e| e.to_string())?;
            let id = w.set_skill(
                a.entity_id,
                &a.name,
                a.rank,
                a.kind.as_deref(),
                a.valid_from.as_deref(),
            )?;
            Ok(format!("skill #{id} set"))
        }
        "add-behavior" => {
            #[derive(serde::Deserialize)]
            struct Args {
                entity_id: i64,
                kind: String,
                trigger: String,
                action: String,
                priority: Option<i64>,
            }
            let a: Args = serde_json::from_value(value).map_err(|e| e.to_string())?;
            let id = w.add_behavior(
                a.entity_id,
                &a.kind,
                &a.trigger,
                &a.action,
                a.priority.unwrap_or(0),
            )?;
            Ok(format!("behavior #{id} added"))
        }
        "add-stress-track" => {
            #[derive(serde::Deserialize)]
            struct Args {
                entity_id: i64,
                name: String,
                size: i64,
            }
            let a: Args = serde_json::from_value(value).map_err(|e| e.to_string())?;
            let id = w.add_stress_track(a.entity_id, &a.name, a.size)?;
            Ok(format!("stress track #{id} added"))
        }
        "mark-stress" => {
            #[derive(serde::Deserialize)]
            struct Args {
                entity_id: i64,
                track: String,
                delta: i64,
            }
            let a: Args = serde_json::from_value(value).map_err(|e| e.to_string())?;
            let marked = w.mark_stress(a.entity_id, &a.track, a.delta)?;
            Ok(format!("{} now {marked}", a.track))
        }
        "clear-stress" => {
            #[derive(serde::Deserialize)]
            struct Args {
                entity_id: i64,
                track: String,
                delta: i64,
            }
            let a: Args = serde_json::from_value(value).map_err(|e| e.to_string())?;
            let marked = w.clear_stress(a.entity_id, &a.track, a.delta)?;
            Ok(format!("{} now {marked}", a.track))
        }
        "join-scene" => {
            #[derive(serde::Deserialize)]
            struct Args {
                entity_id: i64,
                scene_id: i64,
                role: Option<String>,
            }
            let a: Args = serde_json::from_value(value).map_err(|e| e.to_string())?;
            w.join_scene(a.entity_id, a.scene_id, a.role.as_deref())?;
            Ok(format!("joined scene #{}", a.scene_id))
        }
        "leave-scene" => {
            #[derive(serde::Deserialize)]
            struct Args {
                entity_id: i64,
                scene_id: i64,
            }
            let a: Args = serde_json::from_value(value).map_err(|e| e.to_string())?;
            w.leave_scene(a.entity_id, a.scene_id)?;
            Ok(format!("left scene #{}", a.scene_id))
        }
        "begin-scene" => {
            #[derive(serde::Deserialize)]
            struct Args {
                title: String,
                summary: Option<String>,
                location_id: Option<i64>,
                chapter: Option<i64>,
                participants: Option<Vec<Participant>>,
                situation_aspects: Option<Vec<String>>,
            }
            let a: Args = serde_json::from_value(value).map_err(|e| e.to_string())?;
            let id = w.begin_scene(SceneInput {
                title: a.title,
                summary: a.summary,
                location_id: a.location_id,
                chapter: a.chapter,
                participants: a.participants.unwrap_or_default(),
                situation_aspects: a.situation_aspects.unwrap_or_default(),
            })?;
            Ok(format!("scene #{id} begun"))
        }
        "end-scene" => {
            #[derive(serde::Deserialize)]
            struct Args {
                scene_id: i64,
                carry_over: Option<Vec<i64>>,
            }
            let a: Args = serde_json::from_value(value).map_err(|e| e.to_string())?;
            w.end_scene(a.scene_id, &a.carry_over.unwrap_or_default())?;
            Ok(format!("scene #{} ended", a.scene_id))
        }
        "cut-to-scene" => {
            #[derive(serde::Deserialize)]
            struct Args {
                scene_id: i64,
            }
            let a: Args = serde_json::from_value(value).map_err(|e| e.to_string())?;
            w.cut_to_scene(a.scene_id)?;
            Ok(format!("now in scene #{}", a.scene_id))
        }
        "spend-fate" => {
            #[derive(serde::Deserialize)]
            struct Args {
                entity_id: i64,
                amount: i64,
                reason: String,
                aspect_id: Option<i64>,
            }
            let a: Args = serde_json::from_value(value).map_err(|e| e.to_string())?;
            let n = w.spend_fate(a.entity_id, a.amount, &a.reason, a.aspect_id)?;
            Ok(format!("fate balance {n}"))
        }
        "award-fate" => {
            #[derive(serde::Deserialize)]
            struct Args {
                entity_id: i64,
                amount: i64,
                reason: String,
                aspect_id: Option<i64>,
            }
            let a: Args = serde_json::from_value(value).map_err(|e| e.to_string())?;
            let n = w.award_fate(a.entity_id, a.amount, &a.reason, a.aspect_id)?;
            Ok(format!("fate balance {n}"))
        }
        _ => Err(format!("unknown tool `{name}`")),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpg::{store::Store, test_support::test_context};
    #[test]
    fn every_tool_has_a_valid_schema() {
        let schemas = schemas(&[]);
        let mut names = std::collections::HashSet::new();
        for s in &schemas {
            assert_eq!(s["type"], "function");
            let f = &s["function"];
            let name = f["name"].as_str().unwrap();
            assert!(name.chars().next().unwrap().is_ascii_lowercase());
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_lowercase() || c == '-' || c == '_')
            );
            assert!(names.insert(name));
            assert_eq!(f["parameters"]["type"], "object");
            for n in f["parameters"]["required"].as_array().unwrap() {
                assert!(
                    f["parameters"]["properties"]
                        .get(n.as_str().unwrap())
                        .is_some()
                );
            }
        }
        assert!(names.contains("reg_view"));
        assert!(names.contains("reg_search"));
        serde_json::to_string(&schemas).unwrap();
    }
    #[test]
    fn query_results_land_in_registers() {
        let mut c = test_context("rpg-query");
        let s = Store::create(&c.project, "Test").unwrap();
        let mut m = s.meta().clone();
        let mut w = World {
            db: s.db(),
            meta: &mut m,
            changes: vec![],
        };
        let id = w.add_entity("Gareth", "character", None).unwrap();
        w.add_aspect(AspectInput {
            text: "Hot-headed".into(),
            scope: Scope::Character,
            holder_id: Some(id),
            slot: None,
            valid_from: None,
        })
        .unwrap();
        let out = execute(
            &mut c,
            &mut w,
            "aspects",
            &format!("{{\"holder_id\":{id}}}"),
        );
        assert!(out.content.starts_with("→ R4"));
        assert!(c.registers.get(4).unwrap().to_text().contains("Hot-headed"));
        drop(s);
        std::fs::remove_dir_all(c.project.root).unwrap();
    }
    #[test]
    fn mutate_tools_return_short_receipts() {
        let mut c = test_context("rpg-mutate");
        let s = Store::create(&c.project, "Test").unwrap();
        let mut m = s.meta().clone();
        let mut w = World {
            db: s.db(),
            meta: &mut m,
            changes: vec![],
        };
        let out = execute(
            &mut c,
            &mut w,
            "add-aspect",
            r#"{"text":"Empire Falling","scope":"campaign"}"#,
        );
        assert_eq!(out.content, "aspect #1 added");
        assert_eq!(w.aspects(None).unwrap().len(), 1);
        drop(s);
        std::fs::remove_dir_all(c.project.root).unwrap();
    }
    #[test]
    fn bad_arguments_are_errors_not_panics() {
        let mut c = test_context("rpg-bad-args");
        let s = Store::create(&c.project, "Test").unwrap();
        let mut m = s.meta().clone();
        let mut w = World {
            db: s.db(),
            meta: &mut m,
            changes: vec![],
        };
        for (name, args) in [
            ("entity", "{"),
            ("entity", "{}"),
            ("entity", r#"{"id":1,"x":2}"#),
            ("entity", r#"{"id":"abc"}"#),
            ("unknown", "{}"),
        ] {
            let n = s.db().total_changes();
            assert!(execute(&mut c, &mut w, name, args).is_error);
            assert_eq!(s.db().total_changes(), n);
        }
        drop(s);
        std::fs::remove_dir_all(c.project.root).unwrap();
    }
    #[test]
    fn begin_scene_takes_participant_objects() {
        let mut c = test_context("rpg-participant");
        let s = Store::create(&c.project, "Test").unwrap();
        let mut m = s.meta().clone();
        let mut w = World {
            db: s.db(),
            meta: &mut m,
            changes: vec![],
        };
        w.add_entity("A", "character", None).unwrap();
        let out = execute(
            &mut c,
            &mut w,
            "begin-scene",
            r#"{"title":"Bridge","participants":[{"entity_id":1,"role":"pc"}]}"#,
        );
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(
            get_scene_context(s.db(), w.meta, w.meta.current_scene_id, vec![])
                .unwrap()
                .present[0]
                .role,
            "pc"
        );
        drop(s);
        std::fs::remove_dir_all(c.project.root).unwrap();
    }
    #[test]
    fn project_words_are_offered_and_run() {
        let mut c = test_context("rpg-word");
        c.user_words = crate::user_words::compile_one(
            &[],
            "t",
            ": double ( n Int -- r Int ) $n $n + swap drop ;",
        )
        .unwrap();
        assert!(
            schemas(&c.user_words)
                .iter()
                .any(|s| s["function"]["name"] == "double")
        );
        let s = Store::create(&c.project, "Test").unwrap();
        let mut m = s.meta().clone();
        let mut w = World {
            db: s.db(),
            meta: &mut m,
            changes: vec![],
        };
        let out = execute(&mut c, &mut w, "double", r#"{"n":3}"#);
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(c.registers.get(4).unwrap().to_text(), "6");
        drop(s);
        std::fs::remove_dir_all(c.project.root).unwrap();
    }
}

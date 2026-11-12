//! Generation and yes/no decision backends with per-session roles.
use super::store::RoleConfig;
use crate::{
    agent_loop::{self, LoopEvent},
    chat::{self, ChatBackend, StepOptions},
    config::Config,
    events::{Event, Kind},
    project::Project,
    tools::ToolOutcome,
    workflow::Context,
};
use serde_json::{Value as Json, json};
/// One generation request, with optional world tools.
#[derive(Debug, Clone)]
pub struct Task {
    pub system: String,
    pub user: String,
    pub tools: Vec<Json>,
}
/// Tool executor supplied to a generation backend.
pub type Executor<'a> = &'a mut dyn FnMut(&mut Context, &str, &str) -> ToolOutcome;
/// Produces narration or archival text, optionally executing tools.
pub trait Generation: Send {
    fn generate(
        &mut self,
        context: &mut Context,
        task: &Task,
        execute: Executor,
        stream: bool,
    ) -> Result<String, String>;
}
/// A decision with the facts needed to answer it without database access.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DecisionQuery {
    Compel {
        aspect_id: i64,
        aspect_text: String,
        slot: Option<String>,
        scene_id: Option<i64>,
        scene_text: String,
        action_text: String,
    },
    InScope {
        aspect_id: i64,
        aspect_text: String,
        scene_id: Option<i64>,
        scene_text: String,
    },
    Salient {
        candidate: String,
        scene_id: Option<i64>,
        present_names: Vec<String>,
    },
    Tone {
        scene_id: Option<i64>,
        scene_text: String,
    },
}
/// A bounded yes/no answer and optional explanation.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DecisionAnswer {
    pub yes: bool,
    pub confidence: f32,
    pub reason: Option<String>,
}
/// Answers typed world questions without tools.
pub trait Decision: Send {
    fn decide(
        &mut self,
        context: &mut Context,
        query: &DecisionQuery,
    ) -> Result<DecisionAnswer, String>;
    fn name(&self) -> String;
}
/// Deterministic default decisions based on scene words and aspect slots.
pub struct RulesBackend;
/// How a role obtains its chat backend.
pub enum ChatSource {
    Selected,
    Model(String),
    Fixed(Box<dyn ChatBackend + Send>),
}
/// Generates text and grammar-constrained decisions with the selected model.
pub struct LocalLlmBackend {
    pub source: ChatSource,
}
/// Delegates decisions to the configured HTTP endpoint.
pub struct JevBackend {
    pub url: String,
}
/// A parsed role setting, resolved without connecting.
#[derive(Debug, Clone, PartialEq)]
pub enum RoleSpec {
    Local(Option<String>),
    Rules,
    Jev(String),
}
/// The narrator, archivist and compel decider for a session.
pub struct Roles {
    pub narrator: Box<dyn Generation>,
    pub archivist: Box<dyn Generation>,
    pub compel_decider: Box<dyn Decision>,
}
pub const TENSE_WORDS: &[&str] = &[
    "attack",
    "attacks",
    "fight",
    "fights",
    "draw",
    "draws",
    "drew",
    "blade",
    "blades",
    "sword",
    "threat",
    "threaten",
    "threatens",
    "flee",
    "chase",
    "shout",
    "shouts",
    "angry",
    "betray",
    "betrays",
    "betrayed",
    "steal",
    "steals",
    "danger",
    "kill",
    "strike",
    "strikes",
    "duel",
    "ambush",
    "argue",
    "cornered",
    "fire",
    "burning",
];
/// Matches whole tense words in lowercased prose.
pub fn is_tense(text: &str) -> bool {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| TENSE_WORDS.contains(&w))
}
impl Decision for RulesBackend {
    fn decide(&mut self, _: &mut Context, q: &DecisionQuery) -> Result<DecisionAnswer, String> {
        let (yes, confidence, reason) = match q {
            DecisionQuery::Compel {
                slot,
                action_text,
                scene_text,
                ..
            } => {
                let trouble = slot.as_deref() == Some("trouble");
                let tense = is_tense(&format!("{action_text} {scene_text}"));
                (
                    trouble && tense,
                    0.8,
                    Some(
                        if !trouble {
                            "not a trouble aspect"
                        } else if tense {
                            "trouble aspect in a tense scene"
                        } else {
                            "the scene is calm"
                        }
                        .into(),
                    ),
                )
            }
            DecisionQuery::InScope { .. } => (true, 1.0, None),
            DecisionQuery::Salient {
                candidate,
                present_names,
                ..
            } => (
                candidate.starts_with("new entity ")
                    || present_names
                        .iter()
                        .any(|n| candidate.to_lowercase().contains(&n.to_lowercase())),
                0.7,
                None,
            ),
            DecisionQuery::Tone { scene_text, .. } => {
                let tense = is_tense(scene_text);
                (
                    tense,
                    0.8,
                    Some(if tense { "tense" } else { "calm" }.into()),
                )
            }
        };
        Ok(DecisionAnswer {
            yes,
            confidence,
            reason,
        })
    }
    fn name(&self) -> String {
        "rules".into()
    }
}
impl ChatSource {
    /// Resolves the selected backend lazily, preserving the context's server.
    pub fn with<T>(
        &self,
        context: &mut Context,
        f: impl FnOnce(&mut Context, &dyn ChatBackend) -> Result<T, String>,
    ) -> Result<T, String> {
        match self {
            Self::Fixed(b) => f(context, b.as_ref()),
            Self::Selected => {
                let (b, _) = chat::connect(context)?;
                f(context, &b)
            }
            Self::Model(id) => {
                let c = Config::load(&context.project.folder())?;
                if c.model.as_deref() != Some(id) && c.endpoint_model.as_deref() != Some(id) {
                    return Err(format!(
                        "{id} is not the selected model; v1 runs every role on the selected model (use \"local\")"
                    ));
                }
                let (b, _) = chat::connect(context)?;
                f(context, &b)
            }
        }
    }
}
impl Generation for LocalLlmBackend {
    fn generate(
        &mut self,
        c: &mut Context,
        task: &Task,
        execute: Executor,
        stream: bool,
    ) -> Result<String, String> {
        let max = Config::load(&c.project.folder())?.max_steps;
        let mut messages = vec![
            json!({"role":"system","content":task.system}),
            json!({"role":"user","content":task.user}),
        ];
        let mut wrote = false;
        self.source.with(c, |c, b| {
            agent_loop::run_loop(
                c,
                b,
                &mut messages,
                &task.tools,
                max,
                execute,
                &mut |c, event| {
                    if !stream {
                        return;
                    }
                    match event {
                        LoopEvent::Text(t) => {
                            wrote |= !t.is_empty();
                            c.emit(Event::Text(Kind::Chat, t.into()));
                        }
                        LoopEvent::Tool {
                            name,
                            arguments,
                            outcome,
                        } => c.emit(Event::Line(
                            Kind::Note,
                            format!(
                                "⚙ {name} {arguments} {}",
                                outcome.content.lines().next().unwrap_or_default()
                            ),
                        )),
                        LoopEvent::StepDone => {
                            if wrote {
                                c.emit(Event::Text(Kind::Chat, "\n".into()));
                            }
                            wrote = false;
                        }
                        LoopEvent::Thinking => {}
                    }
                },
            )
        })
    }
}
fn parse_answer(text: &str) -> Result<DecisionAnswer, String> {
    let a: DecisionAnswer = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if !(0.0..=1.0).contains(&a.confidence) {
        return Err("confidence must be between 0 and 1".into());
    }
    Ok(a)
}
impl Decision for LocalLlmBackend {
    fn decide(&mut self, c: &mut Context, q: &DecisionQuery) -> Result<DecisionAnswer, String> {
        let mut messages = vec![
            json!({"role":"system","content":prompt(&c.project,"compel")}),
            json!({"role":"user","content":serde_json::to_string(q).map_err(|e|e.to_string())?}),
        ];
        let options = StepOptions {
            grammar: Some(include_str!("decision.gbnf").into()),
            temperature: Some(0.0),
            max_tokens: Some(200),
            no_thinking: true,
            ..Default::default()
        };
        self.source.with(c,|_,b|{for attempt in 0..2 {let t=b.step(&messages,&[],&options,&mut |_|{})?;match parse_answer(&t.content){Ok(a)=>return Ok(a),Err(e) if attempt==0=>{messages.push(json!({"role":"assistant","content":t.content}));messages.push(json!({"role":"user","content":format!("That was not valid: {e}. Reply with only the JSON object.")}));},Err(e)=>return Ok(DecisionAnswer{yes:false,confidence:0.0,reason:Some(format!("dropped: {e}"))})}}Err("decision retry exhausted".into())})
    }
    fn name(&self) -> String {
        "local".into()
    }
}
impl Decision for JevBackend {
    fn decide(&mut self, _: &mut Context, q: &DecisionQuery) -> Result<DecisionAnswer, String> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(std::time::Duration::from_secs(10)))
            .http_status_as_error(false)
            .build()
            .into();
        let mut request = agent.post(&self.url);
        if let Ok(key) = std::env::var("JEV_API_KEY") {
            request = request.header("Authorization", format!("Bearer {key}"));
        }
        let mut response = request
            .send_json(json!({"query":q}))
            .map_err(|e| format!("could not reach jev: {e}"))?;
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|e| e.to_string())?;
        if status != 200 {
            return Err(format!("jev returned {status}: {body}"));
        }
        parse_answer(&body)
    }
    fn name(&self) -> String {
        "jev".into()
    }
}
impl RoleSpec {
    /// Parses a role without starting a model or connecting to a service.
    pub fn parse(text: &str) -> Result<Self, String> {
        if text == "local" {
            return Ok(Self::Local(None));
        }
        if let Some(id) = text.strip_prefix("local:").filter(|s| !s.is_empty()) {
            return Ok(Self::Local(Some(id.into())));
        }
        if text == "rules" {
            return Ok(Self::Rules);
        }
        if let Some(url) = text
            .strip_prefix("jev:")
            .filter(|s| s.starts_with("http://") || s.starts_with("https://"))
        {
            return Ok(Self::Jev(url.into()));
        }
        Err(format!(
            "unknown backend \"{text}\"; use local, local:<id>, rules, or jev:<url>"
        ))
    }
}
impl Roles {
    /// Loads configured role backends, with generation restricted to local models.
    pub fn load(roles: &RoleConfig) -> Result<Self, String> {
        fn generation(role: &str, text: &str) -> Result<Box<dyn Generation>, String> {
            match RoleSpec::parse(text).map_err(|e| format!("meta.json roles.{role}: {e}"))? {
                RoleSpec::Local(id) => Ok(Box::new(LocalLlmBackend {
                    source: id.map(ChatSource::Model).unwrap_or(ChatSource::Selected),
                })),
                _ => Err(format!(
                    "meta.json roles.{role}: {text} cannot generate text; use local or local:<id>"
                )),
            }
        }
        let narrator = generation("narrator", &roles.narrator)?;
        let archivist = generation("archivist", &roles.archivist)?;
        let compel_decider: Box<dyn Decision> = match RoleSpec::parse(&roles.compel_decider)
            .map_err(|e| format!("meta.json roles.compel_decider: {e}"))?
        {
            RoleSpec::Rules => Box::new(RulesBackend),
            RoleSpec::Jev(url) => Box::new(JevBackend { url }),
            RoleSpec::Local(id) => Box::new(LocalLlmBackend {
                source: id.map(ChatSource::Model).unwrap_or(ChatSource::Selected),
            }),
        };
        Ok(Self {
            narrator,
            archivist,
            compel_decider,
        })
    }
}
/// Reads a project prompt override, falling back to the built-in text.
pub fn prompt(project: &Project, name: &str) -> String {
    std::fs::read_to_string(
        project
            .rpg_folder()
            .join("prompts")
            .join(format!("{name}.md")),
    )
    .unwrap_or_else(|_| {
        match name {
            "narrator" => include_str!("prompts/narrator.md"),
            "archivist" => include_str!("prompts/archivist.md"),
            "compel" => include_str!("prompts/compel.md"),
            _ => "",
        }
        .into()
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        chat::{Delta, ToolCall, Turn},
        rpg::test_support::test_context,
    };
    use std::sync::{Arc, Mutex};
    struct Scripted {
        replies: Mutex<Vec<Turn>>,
        seen: Arc<Mutex<Vec<StepOptions>>>,
    }
    impl ChatBackend for Scripted {
        fn step(
            &self,
            _: &[Json],
            _: &[Json],
            options: &StepOptions,
            delta: &mut dyn FnMut(Delta),
        ) -> Result<Turn, String> {
            self.seen.lock().unwrap().push(options.clone());
            let t = self.replies.lock().unwrap().remove(0);
            delta(Delta::Text(t.content.clone()));
            Ok(t)
        }
    }
    fn local(replies: Vec<&str>) -> (LocalLlmBackend, Arc<Mutex<Vec<StepOptions>>>) {
        let seen = Arc::default();
        (
            LocalLlmBackend {
                source: ChatSource::Fixed(Box::new(Scripted {
                    replies: Mutex::new(
                        replies
                            .into_iter()
                            .map(|s| Turn {
                                content: s.into(),
                                ..Default::default()
                            })
                            .collect(),
                    ),
                    seen: Arc::clone(&seen),
                })),
            },
            seen,
        )
    }
    fn compel(slot: &str, action: &str, scene: &str) -> DecisionQuery {
        DecisionQuery::Compel {
            aspect_id: 1,
            aspect_text: "Hot-headed".into(),
            slot: Some(slot.into()),
            scene_id: None,
            scene_text: scene.into(),
            action_text: action.into(),
        }
    }
    #[test]
    fn rules_compel_table() {
        let mut c = test_context("rules");
        for (slot, action, scene, yes) in [
            ("trouble", "I draw my blade", "", true),
            ("trouble", "I sip my tea", "calm", false),
            ("high-concept", "attack", "", false),
            ("trouble", "tea", "Cornered on the bridge", true),
        ] {
            assert_eq!(
                RulesBackend
                    .decide(&mut c, &compel(slot, action, scene))
                    .unwrap()
                    .yes,
                yes
            );
        }
        std::fs::remove_dir_all(c.project.root).unwrap();
    }
    #[test]
    fn rules_salience_and_tone() {
        let mut c = test_context("salient");
        for (candidate, yes) in [
            ("new entity Mara", true),
            ("Aldric promises", true),
            ("the sky is blue", false),
        ] {
            assert_eq!(
                RulesBackend
                    .decide(
                        &mut c,
                        &DecisionQuery::Salient {
                            candidate: candidate.into(),
                            scene_id: None,
                            present_names: vec!["Aldric".into()]
                        }
                    )
                    .unwrap()
                    .yes,
                yes
            );
        }
        assert!(
            RulesBackend
                .decide(
                    &mut c,
                    &DecisionQuery::Tone {
                        scene_id: None,
                        scene_text: "Burning bridge".into()
                    }
                )
                .unwrap()
                .yes
        );
        std::fs::remove_dir_all(c.project.root).unwrap();
    }
    #[test]
    fn local_decision_parses_valid_json() {
        let mut c = test_context("decision");
        let (mut b, seen) = local(vec![r#"{"yes":true,"confidence":0.9,"reason":"fits"}"#]);
        let a = b.decide(&mut c, &compel("trouble", "blade", "")).unwrap();
        assert!(a.yes);
        assert_eq!(a.confidence, 0.9);
        let s = seen.lock().unwrap();
        assert!(s[0].grammar.is_some());
        assert_eq!(s[0].temperature, Some(0.0));
        assert!(s[0].no_thinking);
        std::fs::remove_dir_all(c.project.root).unwrap();
    }
    #[test]
    fn local_decision_retries_once_then_says_no() {
        let mut c = test_context("retry");
        let (mut b, seen) = local(vec!["bad", "bad"]);
        let a = b.decide(&mut c, &compel("trouble", "blade", "")).unwrap();
        assert!(!a.yes);
        assert!(a.reason.unwrap().starts_with("dropped:"));
        assert_eq!(seen.lock().unwrap().len(), 2);
        let (mut b, _) = local(vec![
            "bad",
            r#"{"yes":true,"confidence":1.0,"reason":null}"#,
        ]);
        assert!(
            b.decide(&mut c, &compel("trouble", "blade", ""))
                .unwrap()
                .yes
        );
        std::fs::remove_dir_all(c.project.root).unwrap();
    }
    #[test]
    fn roles_load_resolves_and_locates_errors() {
        Roles::load(&RoleConfig::default()).unwrap();
        Roles::load(&RoleConfig {
            compel_decider: "jev:http://x".into(),
            ..Default::default()
        })
        .unwrap();
        assert!(
            Roles::load(&RoleConfig {
                narrator: "rules".into(),
                ..Default::default()
            })
            .err()
            .unwrap()
            .contains("roles.narrator")
        );
        assert!(
            Roles::load(&RoleConfig {
                compel_decider: "magic".into(),
                ..Default::default()
            })
            .err()
            .unwrap()
            .contains("unknown backend")
        );
    }
    #[test]
    fn prompt_prefers_the_project_copy() {
        let c = test_context("prompt");
        let dir = c.project.rpg_folder().join("prompts");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("narrator.md"), "custom").unwrap();
        assert_eq!(prompt(&c.project, "narrator"), "custom");
        assert!(prompt(&c.project, "archivist").contains("facts"));
        std::fs::remove_dir_all(c.project.root).unwrap();
    }
    #[test]
    fn local_generation_streams_chat_and_runs_tools() {
        let mut c = test_context("generation");
        let recorder = crate::events::Recorder::default();
        let events = recorder.0.clone();
        c.sink = Box::new(recorder);
        let mut b = LocalLlmBackend {
            source: ChatSource::Fixed(Box::new(Scripted {
                replies: Mutex::new(vec![
                    Turn {
                        tool_calls: vec![ToolCall {
                            id: "1".into(),
                            name: "scene".into(),
                            arguments: "{}".into(),
                        }],
                        ..Default::default()
                    },
                    Turn {
                        content: "The bridge groans.".into(),
                        ..Default::default()
                    },
                ]),
                seen: Arc::default(),
            })),
        };
        let mut count = 0;
        let result = b
            .generate(
                &mut c,
                &Task {
                    system: "Narrate".into(),
                    user: "Wait".into(),
                    tools: vec![],
                },
                &mut |_, _, _| {
                    count += 1;
                    ToolOutcome {
                        content: "ok".into(),
                        is_error: false,
                    }
                },
                true,
            )
            .unwrap();
        assert_eq!(result, "The bridge groans.");
        assert_eq!(count, 1);
        let events = events.lock().unwrap();
        assert!(
            events
                .iter()
                .any(|e| matches!(e,Event::Text(Kind::Chat,t) if t.contains("bridge")))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e,Event::Line(Kind::Note,t) if t.starts_with("⚙")))
        );
        std::fs::remove_dir_all(c.project.root).unwrap();
    }
    #[test]
    fn jev_maps_the_http_answer() {
        for status in [200, 500] {
            let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
            let url = format!("http://{}", server.server_addr());
            let thread = std::thread::spawn(move || {
                let mut r = server.recv().unwrap();
                let mut body = String::new();
                r.as_reader().read_to_string(&mut body).unwrap();
                assert_eq!(
                    serde_json::from_str::<Json>(&body).unwrap()["query"]["kind"],
                    "compel"
                );
                r.respond(
                    tiny_http::Response::from_string(
                        r#"{"yes":true,"confidence":0.6,"reason":null}"#,
                    )
                    .with_status_code(status),
                )
                .unwrap();
            });
            let mut c = test_context(&format!("jev-{status}"));
            let a = JevBackend { url }.decide(&mut c, &compel("trouble", "blade", ""));
            if status == 200 {
                assert!(a.unwrap().yes);
            } else {
                assert!(a.unwrap_err().contains("500"));
            }
            thread.join().unwrap();
            std::fs::remove_dir_all(c.project.root).unwrap();
        }
    }
    #[test]
    fn decision_grammar_is_gbnf_not_json_escaped() {
        let grammar = include_str!("decision.gbnf");
        assert!(grammar.starts_with("root   ::= \"{\""));
        assert!(
            grammar
                .lines()
                .any(|line| line == "bool   ::= \"true\" | \"false\"")
        );
    }
}

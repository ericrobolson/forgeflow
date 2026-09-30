use std::io::{BufRead, BufReader};
use std::time::Duration;

use serde_json::{Value, json};

use crate::{config::Config, models, project::SERVER_LOG_FILE, server::LocalServer, workflow::Context};

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// JSON-encoded arguments, exactly as the model produced them.
    pub arguments: String,
}

/// One model response: streamed text plus any tool calls it asked for.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Turn {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: Option<String>,
}

#[derive(Debug, PartialEq)]
pub enum Delta {
    Text(String),
    Reasoning(String),
}

/// Sampling controls for one request. Defaults leave the server's settings alone.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StepOptions {
    /// A GBNF grammar the output must match (llama-server only).
    pub grammar: Option<String>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
    /// Asks templates that support it (such as Qwen3) to skip reasoning.
    pub no_thinking: bool,
    /// Pins the request to a llama-server slot, keeping that slot's prompt cache.
    pub slot: Option<u32>,
}

/// A chat API that can stream a turn and request tool calls.
pub trait ChatBackend {
    fn step(
        &self,
        messages: &[Value],
        tools: &[Value],
        options: &StepOptions,
        on_delta: &mut dyn FnMut(Delta),
    ) -> Result<Turn, String>;
}

/// Any server speaking OpenAI's `/v1/chat/completions`: llama-server, Ollama, LM Studio, vLLM.
pub struct OpenAiCompatible {
    pub endpoint: String,
    pub model: String,
}

impl ChatBackend for OpenAiCompatible {
    fn step(
        &self,
        messages: &[Value],
        tools: &[Value],
        options: &StepOptions,
        on_delta: &mut dyn FnMut(Delta),
    ) -> Result<Turn, String> {
        let mut body = json!({"model": self.model, "messages": messages, "stream": true});
        if !tools.is_empty() {
            body["tools"] = json!(tools);
        }
        if let Some(grammar) = &options.grammar {
            body["grammar"] = json!(grammar);
        }
        if let Some(temperature) = options.temperature {
            body["temperature"] = json!(temperature);
        }
        if let Some(max_tokens) = options.max_tokens {
            body["max_tokens"] = json!(max_tokens);
        }
        if options.no_thinking {
            body["chat_template_kwargs"] = json!({"enable_thinking": false});
        }
        if let Some(slot) = options.slot {
            body["id_slot"] = json!(slot);
        }
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(10)))
            .http_status_as_error(false)
            .build()
            .into();
        let url = format!("{}/chat/completions", self.endpoint.trim_end_matches('/'));
        let mut response = agent
            .post(&url)
            .send_json(&body)
            .map_err(|e| format!("could not reach {url}: {e}"))?;
        if response.status().as_u16() != 200 {
            let status = response.status();
            let text = response.body_mut().read_to_string().unwrap_or_default();
            return Err(format!("{url} returned {status}: {text}"));
        }
        let mut builder = TurnBuilder::default();
        for line in BufReader::new(response.into_body().into_reader()).lines() {
            let line = line.map_err(|e| format!("stream interrupted: {e}"))?;
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data == "[DONE]" {
                break;
            }
            let chunk: Value =
                serde_json::from_str(data).map_err(|e| format!("bad stream chunk `{data}`: {e}"))?;
            if let Some(error) = chunk.get("error") {
                return Err(format!("model server error: {error}"));
            }
            for delta in builder.apply(&chunk) {
                on_delta(delta);
            }
        }
        Ok(builder.finish())
    }
}

/// The configured endpoint, or a llama-server for the selected model (started once per session).
pub fn connect(context: &mut Context) -> Result<(OpenAiCompatible, Config), String> {
    let config = Config::load(&context.project.folder())?;
    if let Some(endpoint) = &config.endpoint {
        let backend = OpenAiCompatible {
            endpoint: endpoint.clone(),
            model: config.endpoint_model.clone().unwrap_or_else(|| "default".into()),
        };
        return Ok((backend, config));
    }
    let id = config.model.as_deref().ok_or(
        "no model selected. Run `forgeflow models` to see options, then `forgeflow pull <id>` and `forgeflow use <id>`",
    )?;
    let (path, _) = models::resolve_installed(id)?;
    if context.server.as_ref().is_none_or(|s| s.model_path != path) {
        context.server = None;
        let log = context.project.folder().join(SERVER_LOG_FILE);
        context.server = Some(LocalServer::start(&path, config.port, config.ctx, &log)?);
    }
    let server = context.server.as_ref().expect("server was just started");
    let backend = OpenAiCompatible {
        endpoint: server.endpoint(),
        model: id.to_string(),
    };
    Ok((backend, config))
}

/// Accumulates streamed chunks; tool call names and arguments arrive in pieces keyed by index.
#[derive(Debug, Default)]
pub struct TurnBuilder {
    turn: Turn,
}

impl TurnBuilder {
    pub fn apply(&mut self, chunk: &Value) -> Vec<Delta> {
        let mut deltas = vec![];
        let Some(choice) = chunk["choices"].get(0) else {
            return deltas;
        };
        let delta = &choice["delta"];
        if let Some(text) = delta["reasoning_content"].as_str().filter(|t| !t.is_empty()) {
            deltas.push(Delta::Reasoning(text.to_string()));
        }
        if let Some(text) = delta["content"].as_str().filter(|t| !t.is_empty()) {
            self.turn.content.push_str(text);
            deltas.push(Delta::Text(text.to_string()));
        }
        for call in delta["tool_calls"].as_array().into_iter().flatten() {
            let index = call["index"].as_u64().unwrap_or(0) as usize;
            while self.turn.tool_calls.len() <= index {
                self.turn.tool_calls.push(ToolCall {
                    id: String::new(),
                    name: String::new(),
                    arguments: String::new(),
                });
            }
            let slot = &mut self.turn.tool_calls[index];
            if let Some(id) = call["id"].as_str() {
                slot.id = id.to_string();
            }
            if let Some(name) = call["function"]["name"].as_str() {
                slot.name.push_str(name);
            }
            if let Some(arguments) = call["function"]["arguments"].as_str() {
                slot.arguments.push_str(arguments);
            }
        }
        if let Some(reason) = choice["finish_reason"].as_str() {
            self.turn.finish_reason = Some(reason.to_string());
        }
        deltas
    }

    pub fn finish(mut self) -> Turn {
        for (index, call) in self.turn.tool_calls.iter_mut().enumerate() {
            if call.id.is_empty() {
                call.id = format!("call_{index}");
            }
        }
        self.turn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulates_text_and_fragmented_tool_calls() {
        let chunks = [
            json!({"choices":[{"delta":{"reasoning_content":"hmm"}}]}),
            json!({"choices":[{"delta":{"content":"Let me "}}]}),
            json!({"choices":[{"delta":{"content":"look."}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"read_file","arguments":"{\"pa"}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"x\"}"}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"name":"list_directory","arguments":"{}"}}]}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
        ];
        let mut builder = TurnBuilder::default();
        let deltas: Vec<Delta> = chunks.iter().flat_map(|c| builder.apply(c)).collect();
        assert_eq!(
            deltas,
            vec![
                Delta::Reasoning("hmm".into()),
                Delta::Text("Let me ".into()),
                Delta::Text("look.".into())
            ]
        );
        let turn = builder.finish();
        assert_eq!(turn.content, "Let me look.");
        assert_eq!(turn.finish_reason.as_deref(), Some("tool_calls"));
        assert_eq!(
            turn.tool_calls,
            vec![
                ToolCall {
                    id: "a".into(),
                    name: "read_file".into(),
                    arguments: "{\"path\":\"x\"}".into()
                },
                ToolCall {
                    id: "call_1".into(),
                    name: "list_directory".into(),
                    arguments: "{}".into()
                },
            ]
        );
    }

    #[test]
    fn ignores_chunks_without_choices() {
        let mut builder = TurnBuilder::default();
        assert!(builder.apply(&json!({"usage":{"total_tokens":3}})).is_empty());
        assert_eq!(builder.finish(), Turn::default());
    }
}

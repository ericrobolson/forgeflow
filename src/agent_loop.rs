use std::collections::HashMap;
use std::io::{IsTerminal, Write};

use serde_json::{Value as Json, json};

use crate::{
    chat::{self, ChatBackend, Delta, StepOptions},
    config::Config,
    models,
    project::Project,
    tools,
    workflow::Context,
};

/// Identical failing calls allowed before the loop gives up on the model.
const MAX_REPEATED_FAILURES: usize = 3;

/// Runs a task on the configured model until it answers without calling tools.
pub fn run(context: &mut Context, task: &str) -> Result<String, String> {
    let config = Config::load(&context.project.folder())?;
    if let Some(model) = config.model.as_deref().and_then(models::find) {
        if !model.tool_calls {
            return Err(format!(
                "{} has no native tool calling, which `agent` needs; select one that does with `forgeflow use <id>`",
                model.id
            ));
        }
    }
    let (backend, config) = chat::connect(context)?;
    let mut stdout = std::io::stdout();
    let styled = stdout.is_terminal();
    let result = run_with(context, &backend, task, config.max_steps, &mut stdout, styled);
    stdout.flush().ok();
    result
}

fn system_prompt(context: &Context) -> String {
    format!(
        "You are a ForgeFlow agent working in the project at {root}. Use the tools to inspect files, then answer the task.\n\
         \n\
         Registers: a tool's result is stored in a scratch register (R4-R15) and you get a short receipt, not the full value. \
         Receipts marked (complete) already contain the whole value. Otherwise use reg_search to find lines, and reg_view to read line ranges.\n\
         - Any text argument can be a register reference such as \"$R5\" to pass that register's value.\n\
         - To show the user a long value, such as a file or edited text, call emit instead of retyping it.\n\
         - R0-R3 are durable notes kept across sessions (max 2000 characters). Store only lasting facts there with reg_store.\n\
         - When scratch registers run out, the least recently used one is replaced and the receipt says so.\n\
         The current register table is appended to the latest message.\n\
         \n\
         When you have the answer, reply in plain text without calling tools.",
        root = context.project.root.display()
    )
}

pub fn run_with(
    context: &mut Context,
    backend: &dyn ChatBackend,
    task: &str,
    max_steps: usize,
    out: &mut dyn Write,
    styled: bool,
) -> Result<String, String> {
    let dim = |text: &str| {
        if styled {
            format!("\x1b[2m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    };
    let schemas = tools::schemas();
    let mut messages = vec![
        json!({"role": "system", "content": system_prompt(context)}),
        json!({"role": "user", "content": task}),
    ];
    let mut failures: HashMap<(String, String), usize> = HashMap::new();
    let result = (|| {
        for _ in 0..max_steps {
            let request = with_register_table(&messages, &context.registers.table());
            let mut thinking_shown = false;
            let mut wrote_text = false;
            let turn = backend.step(&request, &schemas, &StepOptions::default(), &mut |delta| match delta {
                Delta::Text(text) => {
                    wrote_text = true;
                    write!(out, "{text}").ok();
                    out.flush().ok();
                }
                Delta::Reasoning(_) if !thinking_shown => {
                    thinking_shown = true;
                    writeln!(out, "{}", dim("(thinking…)")).ok();
                    out.flush().ok();
                }
                Delta::Reasoning(_) => {}
            })?;
            if wrote_text {
                writeln!(out).ok();
            }

            let mut assistant = json!({"role": "assistant", "content": turn.content});
            if turn.tool_calls.is_empty() {
                messages.push(assistant);
                return Ok(turn.content);
            }
            assistant["tool_calls"] = json!(
                turn.tool_calls
                    .iter()
                    .map(|c| json!({"id": c.id, "type": "function", "function": {"name": c.name, "arguments": c.arguments}}))
                    .collect::<Vec<_>>()
            );
            messages.push(assistant);

            for call in &turn.tool_calls {
                let outcome = tools::execute(context, &call.name, &call.arguments);
                let line = format!("⚙ {} {} {}", call.name, call.arguments, first_line(&outcome.content));
                writeln!(out, "{}", dim(&line)).ok();
                messages.push(json!({"role": "tool", "tool_call_id": call.id, "content": outcome.content}));
                if outcome.is_error {
                    let count = failures
                        .entry((call.name.clone(), call.arguments.clone()))
                        .or_default();
                    *count += 1;
                    if *count >= MAX_REPEATED_FAILURES {
                        return Err(format!(
                            "stopped: the model repeated a failing `{}` call {MAX_REPEATED_FAILURES} times ({})",
                            call.name, outcome.content
                        ));
                    }
                }
            }
        }
        Err(format!(
            "stopped after {max_steps} steps without a final answer. Registers:\n{}",
            context.registers.table()
        ))
    })();
    save_session(context, task, &messages);
    result
}

/// A copy of the history with the register table added to the last message. The stored
/// history never includes it, so earlier messages stay identical and the server's prompt
/// cache keeps matching.
fn with_register_table(messages: &[Json], table: &str) -> Vec<Json> {
    let mut request = messages.to_vec();
    if let Some(last) = request.last_mut() {
        let content = last["content"].as_str().unwrap_or_default();
        last["content"] = json!(format!("{content}\n\n[registers]\n{table}"));
    }
    request
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default()
}

fn save_session(context: &Context, task: &str, messages: &[Json]) {
    let folder = context.project.sessions_folder();
    let path = folder.join(format!("{}.json", uuid::Uuid::now_v7()));
    let transcript = json!({"task": task, "messages": messages});
    let saved = Project::create_ignored_folder(&folder)
        .and_then(|()| std::fs::write(&path, serde_json::to_vec_pretty(&transcript).unwrap_or_default()));
    if let Err(error) = saved {
        eprintln!("could not save the session transcript: {error}");
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::{
        agent::Provider,
        chat::{ToolCall, Turn},
        project::Project,
        registers::Registers,
        value::Value,
    };

    /// Replays scripted turns and records every request it receives.
    struct Scripted {
        turns: RefCell<Vec<Turn>>,
        requests: RefCell<Vec<Vec<Json>>>,
    }

    impl ChatBackend for Scripted {
        fn step(
            &self,
            messages: &[Json],
            _: &[Json],
            _: &StepOptions,
            on_delta: &mut dyn FnMut(Delta),
        ) -> Result<Turn, String> {
            self.requests.borrow_mut().push(messages.to_vec());
            let turn = self.turns.borrow_mut().remove(0);
            if !turn.content.is_empty() {
                on_delta(Delta::Text(turn.content.clone()));
            }
            Ok(turn)
        }
    }

    fn call(name: &str, arguments: &str) -> Turn {
        Turn {
            tool_calls: vec![ToolCall {
                id: format!("id-{name}"),
                name: name.into(),
                arguments: arguments.into(),
            }],
            ..Turn::default()
        }
    }

    fn answer(text: &str) -> Turn {
        Turn {
            content: text.into(),
            ..Turn::default()
        }
    }

    fn project(name: &str) -> Context {
        let root = std::env::temp_dir().join(format!("forgeflow-loop-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("notes.txt"), "the answer is 42").unwrap();
        Context {
            stack: vec![],
            provider: Provider::Codex,
            registers: Registers::in_memory(),
            project: Project { root },
            server: None,
            printed: String::new(),
            store_label: "!".into(),
        }
    }

    #[test]
    fn runs_tools_until_the_model_answers() {
        let mut context = project("answers");
        let backend = Scripted {
            turns: RefCell::new(vec![call("read_file", r#"{"path":"notes.txt"}"#), answer("It is 42.")]),
            requests: RefCell::new(vec![]),
        };
        let mut out = vec![];
        let result = run_with(&mut context, &backend, "what is the answer?", 5, &mut out, false);
        assert_eq!(result, Ok("It is 42.".to_string()));
        assert_eq!(context.registers.get(4).unwrap(), &Value::String("the answer is 42".into()));

        let requests = backend.requests.borrow();
        // The second request carries the tool receipt, and the table rides on the last message only.
        let second = &requests[1];
        let tool_message = second.last().unwrap();
        assert_eq!(tool_message["role"], "tool");
        let content = tool_message["content"].as_str().unwrap();
        assert!(content.starts_with("→ R4 text 16 chars, 1 line \"the answer is 42\" (complete)"), "{content}");
        assert!(content.contains("[registers]"));
        assert!(!second[1]["content"].as_str().unwrap().contains("[registers]"));
        assert!(String::from_utf8(out).unwrap().contains("⚙ read_file"));

        let sessions = context.project.sessions_folder();
        let transcripts = std::fs::read_dir(&sessions)
            .unwrap()
            .filter(|e| e.as_ref().unwrap().path().extension().is_some_and(|x| x == "json"))
            .count();
        assert_eq!(transcripts, 1);
        assert_eq!(std::fs::read_to_string(sessions.join(".gitignore")).unwrap(), "*\n");
        std::fs::remove_dir_all(&context.project.root).unwrap();
    }

    #[test]
    fn stops_when_the_model_repeats_a_failing_call() {
        let mut context = project("repeats");
        let failing = || call("read_file", r#"{"path":"missing.txt"}"#);
        let backend = Scripted {
            turns: RefCell::new(vec![failing(), failing(), failing(), answer("never")]),
            requests: RefCell::new(vec![]),
        };
        let result = run_with(&mut context, &backend, "read it", 10, &mut vec![], false);
        assert!(result.unwrap_err().contains("repeated a failing `read_file` call 3 times"));
        std::fs::remove_dir_all(&context.project.root).unwrap();
    }

    #[test]
    fn stops_at_the_step_limit() {
        let mut context = project("limit");
        let backend = Scripted {
            turns: RefCell::new(vec![call("list_directory", r#"{"path":"."}"#); 3]),
            requests: RefCell::new(vec![]),
        };
        let result = run_with(&mut context, &backend, "loop", 2, &mut vec![], false);
        assert!(result.unwrap_err().starts_with("stopped after 2 steps"));
        std::fs::remove_dir_all(&context.project.root).unwrap();
    }
}

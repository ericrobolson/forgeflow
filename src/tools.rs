use serde_json::{Value as Json, json};

use crate::{
    ops::Ops,
    registers::parse_register,
    type_::TypeKind,
    value::Value,
    workflow::Context,
};

/// What happens to a tool's output.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ResultMode {
    /// Stored in a scratch register; the model gets a receipt.
    Register,
    /// Returned to the model directly.
    Inline,
    /// The word has no output.
    None,
}

/// The words the model may call, and how their results reach it.
fn toolset() -> Vec<(Ops, ResultMode)> {
    vec![
        (Ops::ReadFile, ResultMode::Register),
        (Ops::ListDirectory, ResultMode::Register),
        (Ops::RegView, ResultMode::Inline),
        (Ops::RegSearch, ResultMode::Inline),
        (Ops::RegEdit, ResultMode::Register),
        (Ops::Store, ResultMode::None),
        (Ops::Emit, ResultMode::None),
    ]
}

/// A word's tool name; symbolic words get a callable name.
fn tool_name(op: &Ops) -> &'static str {
    match op {
        Ops::Store => "reg_store",
        other => other.name(),
    }
}

/// Integer parameters are optional; a missing one reaches the word as 0.
fn is_optional(kind: TypeKind) -> bool {
    kind == TypeKind::Int
}

/// OpenAI-style function schemas generated from each word's typed inputs.
pub fn schemas() -> Vec<Json> {
    toolset()
        .iter()
        .map(|(op, _)| {
            let mut properties = serde_json::Map::new();
            let mut required = vec![];
            for input in op.inputs() {
                let schema = match input.kind {
                    TypeKind::Int => json!({"type": "integer", "description": input.description}),
                    TypeKind::Bool => json!({"type": "boolean", "description": input.description}),
                    TypeKind::Register => json!({
                        "type": "string",
                        "description": format!("{} (R0-R15)", input.description)
                    }),
                    TypeKind::String | TypeKind::Any | TypeKind::List => json!({
                        "type": "string",
                        "description": format!("{}; \"$R5\" passes register R5's value", input.description)
                    }),
                };
                properties.insert(input.name.to_string(), schema);
                if !is_optional(input.kind) {
                    required.push(input.name);
                }
            }
            json!({
                "type": "function",
                "function": {
                    "name": tool_name(op),
                    "description": op.description(),
                    "parameters": {"type": "object", "properties": properties, "required": required}
                }
            })
        })
        .collect()
}

#[derive(Debug, PartialEq)]
pub struct ToolOutcome {
    pub content: String,
    pub is_error: bool,
}

/// Runs one tool call on the context's registers, returning what the model sees.
pub fn execute(context: &mut Context, name: &str, arguments: &str) -> ToolOutcome {
    match try_execute(context, name, arguments) {
        Ok(content) => ToolOutcome {
            content,
            is_error: false,
        },
        Err(error) => ToolOutcome {
            content: format!("error: {error}"),
            is_error: true,
        },
    }
}

fn try_execute(context: &mut Context, name: &str, arguments: &str) -> Result<String, String> {
    let (op, mode) = toolset()
        .into_iter()
        .find(|(op, _)| tool_name(op) == name)
        .ok_or_else(|| format!("unknown tool `{name}`"))?;
    let arguments: Json = if arguments.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(arguments).map_err(|e| format!("arguments are not valid JSON: {e}"))?
    };

    let mut inputs = vec![];
    let mut labels = vec![];
    for input in op.inputs() {
        let argument = &arguments[input.name];
        if argument.is_null() && !is_optional(input.kind) {
            return Err(format!("missing argument `{}`", input.name));
        }
        let value = to_value(context, input.kind, argument)
            .map_err(|e| format!("argument `{}`: {e}", input.name))?;
        if let Some(label) = argument.as_str() {
            labels.push(shorten(label));
        }
        inputs.push(value);
    }
    let source = format!("{name} {}", labels.join(" ")).trim().to_string();

    // Run the word on a private stack so tool calls never disturb the program's stack.
    let saved = std::mem::replace(&mut context.stack, inputs);
    let result = op.process(context);
    let outputs = std::mem::replace(&mut context.stack, saved);
    result?;

    match mode {
        ResultMode::Register => {
            let value = outputs.into_iter().last().ok_or("the word produced no value")?;
            let allocation = context.registers.allocate(value, &source);
            let mut receipt = format!("→ {}", context.registers.receipt(allocation.register));
            if let Some(replaced) = allocation.replaced {
                receipt.push_str(&format!(" (replaced: {replaced})"));
            }
            Ok(receipt)
        }
        ResultMode::Inline => Ok(outputs.into_iter().map(|v| v.to_text()).collect::<Vec<_>>().join("\n")),
        ResultMode::None => Ok(match op {
            Ops::Store => {
                let register = register_argument(&arguments);
                context.registers.relabel(register, &source)?;
                format!("stored; {}", context.registers.receipt(register))
            }
            Ops::Emit => "shown to the user".to_string(),
            _ => "done".to_string(),
        }),
    }
}

fn register_argument(arguments: &Json) -> usize {
    arguments["register"]
        .as_str()
        .and_then(parse_register)
        .or_else(|| arguments["register"].as_u64().map(|r| r as usize))
        .unwrap_or(0)
}

/// Converts a JSON argument to a typed value, resolving `$R5` references.
fn to_value(context: &mut Context, kind: TypeKind, argument: &Json) -> Result<Value, String> {
    let reference = argument
        .as_str()
        .filter(|s| s.starts_with('$'))
        .and_then(parse_register);
    match kind {
        TypeKind::Register => argument
            .as_str()
            .and_then(parse_register)
            .or_else(|| argument.as_u64().map(|r| r as usize))
            .map(Value::Register)
            .ok_or_else(|| format!("expected a register such as R5, got {argument}")),
        TypeKind::Int => match argument {
            Json::Null => Ok(Value::Int(0)),
            Json::Number(n) => n.as_i64().map(Value::Int).ok_or("expected an integer".into()),
            Json::String(s) => s.trim().parse().map(Value::Int).map_err(|_| format!("expected an integer, got {s:?}")),
            other => Err(format!("expected an integer, got {other}")),
        },
        TypeKind::Bool => argument.as_bool().map(Value::Bool).ok_or("expected true or false".into()),
        TypeKind::String | TypeKind::List => match reference {
            Some(register) => Ok(Value::String(context.registers.get(register)?.to_text())),
            None => argument
                .as_str()
                .map(|s| Value::String(s.to_string()))
                .ok_or_else(|| format!("expected a string, got {argument}")),
        },
        TypeKind::Any => match (reference, argument) {
            (Some(register), _) => Ok(context.registers.get(register)?.clone()),
            (None, Json::String(s)) => Ok(Value::String(s.clone())),
            (None, Json::Number(n)) => n.as_i64().map(Value::Int).ok_or("expected an integer".into()),
            (None, Json::Bool(b)) => Ok(Value::Bool(*b)),
            (None, other) => Err(format!("unsupported value {other}")),
        },
    }
}

fn shorten(text: &str) -> String {
    const MAX: usize = 60;
    let line = text.replace('\n', "⏎");
    if line.chars().count() <= MAX {
        line
    } else {
        format!("{}…", line.chars().take(MAX).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{agent::Provider, project::Project, registers::Registers};

    fn context_in(root: &std::path::Path) -> Context {
        Context {
            stack: vec![Value::Int(99)],
            provider: Provider::Codex,
            registers: Registers::in_memory(),
            project: Project {
                root: root.to_path_buf(),
            },
            server: None,
            printed: String::new(),
            store_label: "!".into(),
        }
    }

    fn temp_project(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("forgeflow-tools-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {\n    println!(\"hi\");\n}\n").unwrap();
        std::fs::write(root.join("README.md"), "readme").unwrap();
        root
    }

    #[test]
    fn schemas_come_from_word_inputs() {
        let schemas = schemas();
        let names: Vec<_> = schemas.iter().map(|s| s["function"]["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            ["read_file", "list_directory", "reg_view", "reg_search", "reg_edit", "reg_store", "emit"]
        );
        let reg_view = &schemas[2]["function"]["parameters"];
        assert_eq!(reg_view["required"], json!(["register"]));
        assert_eq!(reg_view["properties"]["start"]["type"], "integer");
    }

    #[test]
    fn results_land_in_registers_and_the_program_stack_is_untouched() {
        let root = temp_project("receipts");
        let mut context = context_in(&root);
        let outcome = execute(&mut context, "read_file", r#"{"path":"src/main.rs"}"#);
        assert!(!outcome.is_error, "{}", outcome.content);
        assert!(outcome.content.starts_with("→ R4 text 34 chars, 3 lines"), "{}", outcome.content);
        assert_eq!(context.stack, vec![Value::Int(99)]);

        let listing = execute(&mut context, "list_directory", r#"{"path":"."}"#);
        assert!(listing.content.starts_with("→ R5 list(2)"), "{}", listing.content);
        assert_eq!(
            context.registers.get(5).unwrap(),
            &Value::List(vec![Value::String("src/".into()), Value::String("README.md".into())])
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn register_references_edit_and_view() {
        let root = temp_project("edit");
        let mut context = context_in(&root);
        execute(&mut context, "read_file", r#"{"path":"src/main.rs"}"#);
        let edit = execute(
            &mut context,
            "reg_edit",
            r#"{"register":"R4","find":"\"hi\"","replace":"\"bye\""}"#,
        );
        assert!(edit.content.starts_with("→ R5"), "{}", edit.content);
        let view = execute(&mut context, "reg_view", r#"{"register":"R5","start":2,"end":2}"#);
        assert_eq!(view.content, "2\t    println!(\"bye\");\n(1 more lines)");

        let store = execute(&mut context, "reg_store", r#"{"value":"$R5","register":"R6"}"#);
        assert!(store.content.starts_with("stored; R6 text"), "{}", store.content);
        let copied = context.registers.get(5).unwrap().clone();
        assert_eq!(context.registers.get(6).unwrap(), &copied);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn errors_are_reported_to_the_model_not_raised() {
        let root = temp_project("errors");
        let mut context = context_in(&root);
        let cases = [
            ("nope", "{}", "unknown tool"),
            ("read_file", "{}", "missing argument `path`"),
            ("read_file", "not json", "not valid JSON"),
            ("read_file", r#"{"path":"../secret"}"#, "leaves the project root"),
            ("reg_view", r#"{"register":"R9"}"#, "R9 is empty"),
            ("reg_view", r#"{"register":"X1"}"#, "expected a register"),
        ];
        for (name, arguments, expected) in cases {
            let outcome = execute(&mut context, name, arguments);
            assert!(outcome.is_error, "{name}");
            assert!(outcome.content.contains(expected), "{name}: {}", outcome.content);
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}

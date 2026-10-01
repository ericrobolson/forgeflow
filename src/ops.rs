use std::io::Write;

use crate::{
    agent::{self, Agent},
    agent_loop,
    chat::{self, ChatBackend, Delta, StepOptions},
    config::Config,
    device::Device,
    image_gen::ImageBackend,
    models,
    type_::{Type, TypeKind},
    util,
    value::Value,
    workflow::*,
};

/// The most lines `reg_view` returns at once.
pub const VIEW_MAX_LINES: usize = 200;
const VIEW_MAX_BYTES: usize = 16 * 1024;
const READ_MAX_BYTES: u64 = 2_000_000;
/// Tokens of the context window kept free for `answer`'s instructions and reply.
const ANSWER_RESERVED_TOKENS: u64 = 1500;
/// A lower bound on characters per token, so trimmed text always fits the context.
const ANSWER_CHARS_PER_TOKEN: u64 = 2;

#[derive(Debug, Clone, PartialEq)]
pub enum Ops {
    SpecifyAgent(agent::Provider),
    StringLiteral(String),
    IntLiteral(i64),
    BoolLiteral(bool),
    /// Pushes a register reference such as `R5`.
    Register(usize),
    AllCliArgs,
    WriteFile,
    /// Reads a line from stdin
    ReadLine,
    AskLlm,
    Print,
    Dup,
    Drop,
    Swap,
    /// `@`: pushes a register's value.
    Fetch,
    /// `!`: pops a value into a register.
    Store,
    ReadFile,
    /// Generate one image and save it below the project root.
    MakeImage,
    ListDirectory,
    /// Files only, without folders.
    ListFiles,
    /// `[ ... ] each`: runs the block once per list item, with the item on the stack.
    Each(Vec<Ops>),
    LocalGet(String, TypeKind),
    AddressOf(String),
    UserCall { name: String, inputs: Vec<TypeKind>, outputs: Vec<TypeKind> },
    RegView,
    RegSearch,
    /// Lines of text containing a pattern.
    Grep,
    /// Streams a model reply to a question about some text.
    Answer,
    /// Explains what a message can ask for.
    Help,
    Add,
    Subtract,
    Multiply,
    Divide,
    RegEdit,
    Emit,
    ShowRegisters,
    Models,
    Pull,
    UseModel,
    /// Runs the tool-calling agent loop on a task.
    Agent,
}

impl Ops {
    pub fn token_name(&self) -> String {
        match self { Ops::UserCall { name, .. } => name.clone(), _ => self.name().to_string() }
    }
    /// Every word callable by name, for the compiler's lookup table.
    pub fn named_words() -> Vec<Ops> {
        vec![
            Ops::WriteFile,
            Ops::ReadLine,
            Ops::AskLlm,
            Ops::Print,
            Ops::Dup,
            Ops::Drop,
            Ops::Swap,
            Ops::Fetch,
            Ops::Store,
            Ops::ReadFile,
            Ops::MakeImage,
            Ops::ListDirectory,
            Ops::ListFiles,
            Ops::RegView,
            Ops::RegSearch,
            Ops::Grep,
            Ops::Answer,
            Ops::Help,
            Ops::Add,
            Ops::Subtract,
            Ops::Multiply,
            Ops::Divide,
            Ops::RegEdit,
            Ops::Emit,
            Ops::ShowRegisters,
            Ops::Models,
            Ops::Pull,
            Ops::UseModel,
            Ops::Agent,
        ]
    }

    pub fn name(&self) -> &'static str {
        match self {
            Ops::SpecifyAgent(_) => "specify_agent",
            Ops::StringLiteral(_) => "string literal",
            Ops::IntLiteral(_) => "integer literal",
            Ops::BoolLiteral(_) => "boolean literal",
            Ops::Register(_) => "register",
            Ops::AllCliArgs => "all_cli_args",
            Ops::WriteFile => "write_file",
            Ops::ReadLine => "read_line",
            Ops::AskLlm => "ask",
            Ops::Print => "print",
            Ops::Dup => "dup",
            Ops::Drop => "drop",
            Ops::Swap => "swap",
            Ops::Fetch => "@",
            Ops::Store => "!",
            Ops::ReadFile => "read_file",
            Ops::MakeImage => "make-image",
            Ops::ListDirectory => "list_directory",
            Ops::ListFiles => "list_files",
            Ops::Each(_) => "each",
            Ops::LocalGet(_, _) => "local input",
            Ops::AddressOf(_) => "address",
            Ops::UserCall { .. } => "user word",
            Ops::RegView => "reg_view",
            Ops::RegSearch => "reg_search",
            Ops::Grep => "grep",
            Ops::Answer => "answer",
            Ops::Help => "help",
            Ops::Add => "+",
            Ops::Subtract => "-",
            Ops::Multiply => "*",
            Ops::Divide => "/",
            Ops::RegEdit => "reg_edit",
            Ops::Emit => "emit",
            Ops::ShowRegisters => "registers",
            Ops::Models => "models",
            Ops::Pull => "pull",
            Ops::UseModel => "use_model",
            Ops::Agent => "agent",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Ops::ReadFile => "Read a UTF-8 file. The path is relative to the project root.",
            Ops::ListDirectory => {
                "List a folder relative to the project root; use . for the root. Folders end with / and come first."
            }
            Ops::ListFiles => "List the files (not folders) in a folder relative to the project root, alphabetically.",
            Ops::MakeImage => "Generate one image with OpenRouter and save it inside the project, using the image model's default dimensions and returned file format.",
            Ops::Each(_) => "Run the [ ] block once per item of a list, with the item on the stack.",
            Ops::RegView => {
                "Show numbered lines of a register's text. start and end are 1-based and inclusive; at most 200 lines."
            }
            Ops::RegSearch => {
                "Find lines in a register's text containing `pattern` (case-insensitive), with line numbers."
            }
            Ops::RegEdit => {
                "Replace exactly one occurrence of `find` in a register's text; the edited text goes to a new register."
            }
            Ops::Store => {
                "Store a value in a register. R0-R3 are durable notes kept across sessions (max 2000 characters)."
            }
            Ops::Emit => "Show a register's full contents to the user, instead of retyping them.",
            Ops::Agent => "Run the local model on a task, letting it call tools.",
            Ops::Grep => "Keep the lines of text that contain a pattern (case-insensitive), numbered.",
            Ops::Answer => {
                "Reply to the user in prose about some context text; use \"\" as the context for general questions."
            }
            Ops::Print => "Show a value to the user.",
            Ops::Help => "Explain what you can ask for.",
            Ops::Add => "Add two integers.",
            Ops::Subtract => "Subtract the top integer from the one below it.",
            Ops::Multiply => "Multiply two integers.",
            Ops::Divide => "Divide the integer below by the top one, rounding toward zero.",
            Ops::Fetch => "Push a register's value.",
            Ops::ShowRegisters => "Show all registers.",
            Ops::Dup => "Copy the top value.",
            Ops::Swap => "Swap the top two values.",
            Ops::Drop => "Discard the top value.",
            Ops::Models => "List local models with size, fit, and speed estimates for this device.",
            Ops::Pull => "Download a catalogued model.",
            Ops::UseModel => "Select the model the agent runs.",
            _ => "",
        }
    }

    pub fn process(&self, context: &mut Context) -> Result<(), String> {
        match self {
            Ops::AllCliArgs => todo!(),
            Ops::WriteFile => {
                let contents = pop(context)?.expect_string()?;
                let path = pop(context)?.expect_string()?;
                let path = context.project.resolve(&path)?;
                std::fs::write(&path, contents).map_err(|e| format!("{}: {e}", path.display()))?;
            }
            Ops::MakeImage => {
                let prompt = pop(context)?.expect_string()?;
                let relative = pop(context)?.expect_string()?;
                let requested_path = std::path::Path::new(relative.trim());
                if !matches!(requested_path.components().next_back(), Some(std::path::Component::Normal(_))) {
                    return Err("image output path must include a filename".into());
                }
                let png_relative = requested_path.with_extension("png");
                let png_relative = png_relative
                    .to_str()
                    .ok_or("image output path is not valid UTF-8")?;
                let path = context.project.resolve(png_relative)?;
                let config = Config::load(&context.project.folder())?;
                let image = crate::image_gen::OpenRouterImageBackend::from_environment(&config.image_model)?
                    .generate(&prompt)?;
                let saved_path = crate::image_gen::save_image(&path, &image)?;
                let saved_relative = saved_path.strip_prefix(&context.project.root)
                    .map_err(|_| "generated image path is outside the project".to_string())?;
                context.stack.push(Value::String(saved_relative.to_string_lossy().into_owned()));
            }
            Ops::ReadLine => {
                let answer = util::read_line()?;
                context.stack.push(Value::String(answer));
            }
            Ops::AskLlm => {
                let question = pop(context)?.expect_string()?;
                let agent = Agent::new(context.provider);
                let answer = agent.ask(&question).map_err(|e| e.to_string())?;
                context.stack.push(Value::String(answer));
            }
            Ops::Print => {
                let value = pop(context)?;
                context.say(&value.to_text());
            }
            Ops::StringLiteral(s) => context.stack.push(Value::String(s.clone())),
            Ops::IntLiteral(i) => context.stack.push(Value::Int(*i)),
            Ops::BoolLiteral(b) => context.stack.push(Value::Bool(*b)),
            Ops::Register(r) => context.stack.push(Value::Register(*r)),
            Ops::SpecifyAgent(provider) => todo!(),
            Ops::LocalGet(name, _) => {
                let value = context.local_frames.iter().rev().find_map(|frame| frame.get(name)).cloned()
                    .ok_or_else(|| format!("no active input named `${name}`"))?;
                context.stack.push(value);
            }
            Ops::AddressOf(name) => context.stack.push(context.memory.address(name)),
            Ops::UserCall { name, .. } => crate::user_words::execute(context, name)?,
            Ops::Add | Ops::Subtract | Ops::Multiply | Ops::Divide => {
                let b = pop(context)?.expect_int()?;
                let a = pop(context)?.expect_int()?;
                context.stack.push(Value::Int(arithmetic(self, a, b)?));
            }
            Ops::Dup => {
                let top = context.stack.last().cloned().ok_or("dup needs a value")?;
                context.stack.push(top);
            }
            Ops::Drop => {
                pop(context)?;
            }
            Ops::Swap => {
                let top = pop(context)?;
                let below = pop(context)?;
                context.stack.push(top);
                context.stack.push(below);
            }
            Ops::Fetch => {
                let value = pop(context)?;
                let value = match value {
                    Value::Register(register) => context.registers.get(register)?.clone(),
                    Value::Address(name) => context.memory.get(&name)?.clone(),
                    other => return Err(format!("@ needs a register or address, got {}", other.describe())),
                };
                context.stack.push(value);
            }
            Ops::Store => {
                let target = pop(context)?;
                let value = pop(context)?;
                let label = context.store_label.clone();
                match target {
                    Value::Register(register) => context.registers.set(register, value, &label)?,
                    Value::Address(name) => context.memory.set(&name, value)?,
                    other => return Err(format!("! needs a register or address, got {}", other.describe())),
                }
            }
            Ops::ReadFile => {
                let relative = pop(context)?.expect_string()?;
                let path = context.project.resolve(&relative)?;
                let size = std::fs::metadata(&path)
                    .map_err(|e| format!("{relative}: {e}"))?
                    .len();
                if size > READ_MAX_BYTES {
                    return Err(format!("{relative} is {size} bytes; the limit is {READ_MAX_BYTES}"));
                }
                let text = std::fs::read_to_string(&path).map_err(|e| format!("{relative}: {e}"))?;
                context.stack.push(Value::String(text));
            }
            Ops::ListDirectory => {
                let relative = pop(context)?.expect_string()?;
                let path = context.project.resolve(&relative)?;
                let entries = list_directory(&path).map_err(|e| format!("{relative}: {e}"))?;
                context
                    .stack
                    .push(Value::List(entries.into_iter().map(Value::String).collect()));
            }
            Ops::ListFiles => {
                let relative = pop(context)?.expect_string()?;
                let path = context.project.resolve(&relative)?;
                let entries = list_directory(&path).map_err(|e| format!("{relative}: {e}"))?;
                let files = entries.into_iter().filter(|e| !e.ends_with('/')).map(Value::String);
                context.stack.push(Value::List(files.collect()));
            }
            Ops::Each(body) => {
                let items = match pop(context)? {
                    Value::List(items) => items,
                    other => return Err(format!("`each` needs a list, got {}", other.describe())),
                };
                for item in items {
                    let depth = context.stack.len();
                    let label = item.to_text();
                    context.stack.push(item);
                    // One item failing does not stop the others.
                    if let Err(error) = crate::workflow::run_steps(context, body) {
                        context.stack.truncate(depth);
                        context.say(&format!("error ({label}): {error}"));
                    }
                }
            }
            Ops::RegView => {
                let end = pop(context)?.expect_int()?;
                let start = pop(context)?.expect_int()?;
                let register = pop(context)?.expect_register()?;
                let text = context.registers.get(register)?.to_text();
                context.stack.push(Value::String(view_lines(&text, start, end)));
            }
            Ops::RegSearch => {
                let pattern = pop(context)?.expect_string()?;
                let register = pop(context)?.expect_register()?;
                let text = context.registers.get(register)?.to_text();
                context.stack.push(Value::String(search_lines(&text, &pattern)));
            }
            Ops::RegEdit => {
                let replace = pop(context)?.expect_string()?;
                let find = pop(context)?.expect_string()?;
                let register = pop(context)?.expect_register()?;
                let text = context.registers.get(register)?.to_text();
                context
                    .stack
                    .push(Value::String(replace_once(&text, &find, &replace, register)?));
            }
            Ops::Emit => {
                let register = pop(context)?.expect_register()?;
                let text = context.registers.get(register)?.to_text();
                context.say(&text);
            }
            Ops::ShowRegisters => {
                let table = context.registers.table();
                context.say(&table);
            }
            Ops::Models => {
                let config = Config::load(&context.project.folder())?;
                let table = models::render_table(&Device::probe(), config.ctx, config.model.as_deref());
                context.say(&table);
            }
            Ops::Grep => {
                let pattern = pop(context)?.expect_string()?;
                let text = pop(context)?.expect_string()?;
                context.stack.push(Value::String(search_lines(&text, &pattern)));
            }
            Ops::Help => {
                let text = crate::translate::help_text();
                context.say(&text);
            }
            Ops::Answer => {
                let question = pop(context)?.expect_string()?;
                let text = pop(context)?.expect_string()?;
                answer(context, &text, &question)?;
            }
            Ops::Pull => {
                let id = pop(context)?.expect_string()?;
                models::pull(&id, &mut std::io::stderr())?;
            }
            Ops::UseModel => {
                let id = pop(context)?.expect_string()?;
                let (path, catalogued) = models::resolve_installed(&id)?;
                // Chat works with any model (a grammar shapes its output); only `agent` needs tool calls.
                match &catalogued {
                    Some(model) if !model.tool_calls => {
                        eprintln!("{id} has no native tool calling: chat works, but the `agent` word will not.")
                    }
                    None => eprintln!("{id} is not catalogued; the `agent` word may not work with it."),
                    _ => {}
                }
                let folder = context.project.folder();
                let mut config = Config::load(&folder)?;
                config.model = Some(id.clone());
                config.save(&folder)?;
                // A running server holds the previous model; the next `agent` restarts it.
                context.server = None;
                println!("Selected {id} ({})", path.display());
            }
            Ops::Agent => {
                let task = pop(context)?.expect_string()?;
                let answer = agent_loop::run(context, &task)?;
                context.stack.push(Value::String(answer));
            }
        }
        std::io::stdout().flush().ok();
        Ok(())
    }

    /// The things the given step requires on the stack, listed bottom to top.
    pub fn inputs(&self) -> Vec<Type> {
        use TypeKind::*;
        match self {
            Ops::WriteFile => vec![Type::str("path", "file path"), Type::str("contents", "contents")],
            Ops::AskLlm => vec![Type::str("question", "the question to ask")],
            Ops::Print => vec![Type::new("value", Any, "the value to print")],
            Ops::Dup | Ops::Drop => vec![Type::new("value", Any, "a value")],
            Ops::Swap => vec![Type::new("a", Any, "a value"), Type::new("b", Any, "a value")],
            Ops::Fetch => vec![Type::new("address", Reference, "a register or address")],
            Ops::Emit => vec![Type::new("register", Register, "a register such as R5")],
            Ops::Store => vec![
                Type::new("value", Any, "the value, or $R5 to copy a register"),
                Type::new("register", Reference, "the register or address to store into"),
            ],
            Ops::ReadFile | Ops::ListDirectory | Ops::ListFiles => {
                vec![Type::str("path", "path relative to the project root")]
            }
            Ops::MakeImage => vec![
                Type::str("path", "requested output path relative to the project root; extension is adjusted to match the generated format"),
                Type::str("prompt", "description of the image to generate"),
            ],
            Ops::Each(_) => vec![Type::new("items", List, "the list to go through")],
            Ops::RegView => vec![
                Type::new("register", Register, "a register such as R5"),
                Type::new("start", Int, "first line, from 1"),
                Type::new("end", Int, "last line, inclusive"),
            ],
            Ops::RegSearch => vec![
                Type::new("register", Register, "a register such as R5"),
                Type::str("pattern", "text to look for"),
            ],
            Ops::Grep => vec![Type::str("text", "text to search"), Type::str("pattern", "text to look for")],
            Ops::Add | Ops::Subtract | Ops::Multiply | Ops::Divide => {
                vec![Type::new("a", Int, "an integer"), Type::new("b", Int, "an integer")]
            }
            Ops::Answer => vec![
                Type::str("context", "text the reply is about, or \"\""),
                Type::str("question", "what to reply to"),
            ],
            Ops::RegEdit => vec![
                Type::new("register", Register, "the register holding the text"),
                Type::str("find", "text that occurs exactly once"),
                Type::str("replace", "replacement text"),
            ],
            Ops::Pull | Ops::UseModel => vec![Type::str("id", "a model ID from `models`")],
            Ops::Agent => vec![Type::str("task", "what the agent should do")],
            Ops::ReadLine
            | Ops::StringLiteral(_)
            | Ops::IntLiteral(_)
            | Ops::BoolLiteral(_)
            | Ops::Register(_)
            | Ops::SpecifyAgent(_)
            | Ops::AllCliArgs
            | Ops::ShowRegisters
            | Ops::Help
            | Ops::Models => vec![],
            Ops::LocalGet(_, _) => vec![],
            Ops::AddressOf(_) => vec![],
            Ops::UserCall { inputs, .. } => inputs.iter().map(|kind| Type::new("input", *kind, "user word input")).collect(),
        }
    }

    /// The things the given step puts on the stack.
    pub fn outputs(&self) -> Vec<Type> {
        use TypeKind::*;
        match self {
            Ops::AllCliArgs => vec![Type::str("args", "the args as a string")],
            Ops::ReadLine => vec![Type::str("line", "A line the user wrote")],
            Ops::AskLlm => vec![Type::str("response", "The response")],
            Ops::StringLiteral(_) => vec![Type::str("literal", "The string literal")],
            Ops::IntLiteral(_) => vec![Type::new("literal", Int, "The integer literal")],
            Ops::BoolLiteral(_) => vec![Type::new("literal", Bool, "The boolean literal")],
            Ops::Register(_) => vec![Type::new("register", Register, "The register")],
            Ops::Dup => vec![Type::new("value", Any, "a value"), Type::new("copy", Any, "a value")],
            Ops::Swap => vec![Type::new("b", Any, "a value"), Type::new("a", Any, "a value")],
            Ops::Fetch => vec![Type::new("value", Any, "the register's value")],
            Ops::ReadFile => vec![Type::str("text", "the file's contents")],
            Ops::MakeImage => vec![Type::str("saved_path", "project-relative path of the generated image")],
            Ops::ListDirectory => vec![Type::new("entries", List, "the folder's entries")],
            Ops::ListFiles => vec![Type::new("files", List, "the folder's files")],
            Ops::RegView | Ops::RegSearch | Ops::Grep => vec![Type::str("lines", "numbered lines")],
            Ops::Add | Ops::Subtract | Ops::Multiply | Ops::Divide => {
                vec![Type::new("result", Int, "the result")]
            }
            Ops::RegEdit => vec![Type::str("text", "the edited text")],
            Ops::Agent => vec![Type::str("answer", "the agent's final answer")],
            Ops::SpecifyAgent(_)
            | Ops::WriteFile
            | Ops::Print
            | Ops::Drop
            | Ops::Store
            | Ops::Emit
            | Ops::Each(_)
            | Ops::Answer
            | Ops::Help
            | Ops::ShowRegisters
            | Ops::Models
            | Ops::Pull
            | Ops::UseModel => vec![],
            Ops::LocalGet(_, kind) => vec![Type::new("local", *kind, "named input")],
            Ops::AddressOf(_) => vec![Type::new("address", Address, "durable memory address")],
            Ops::UserCall { outputs, .. } => outputs.iter().map(|kind| Type::new("output", *kind, "user word output")).collect(),
        }
    }
}

/// Checked integer arithmetic: overflow and division by zero are errors, not wraparound.
fn arithmetic(op: &Ops, a: i64, b: i64) -> Result<i64, String> {
    let result = match op {
        Ops::Add => a.checked_add(b),
        Ops::Subtract => a.checked_sub(b),
        Ops::Multiply => a.checked_mul(b),
        Ops::Divide if b == 0 => return Err("division by zero".into()),
        Ops::Divide => a.checked_div(b),
        _ => unreachable!("arithmetic called with {}", op.name()),
    };
    result.ok_or_else(|| format!("{a} {} {b} overflows", op.name()))
}

/// Streams a reply from the local model about `text`, trimming text the context window cannot hold.
fn answer(context: &mut Context, text: &str, question: &str) -> Result<(), String> {
    let (backend, config) = chat::connect(context)?;
    // Leave room for the reply. Dense text such as lockfile hashes runs near two
    // characters per token, so this bound holds for it too.
    let max_chars = (config.ctx.saturating_sub(ANSWER_RESERVED_TOKENS) * ANSWER_CHARS_PER_TOKEN) as usize;
    let trimmed = text.chars().count() > max_chars;
    let mut text: String = text.chars().take(max_chars).collect();
    if trimmed {
        text.push_str("\n[trimmed]");
    }
    let prompt = if text.trim().is_empty() {
        question.to_string()
    } else {
        format!("{question}\n\n---\n{text}")
    };
    let messages = [
        serde_json::json!({"role": "system", "content": "Reply concisely and directly. Base your reply on the provided text when there is one."}),
        serde_json::json!({"role": "user", "content": prompt}),
    ];
    let options = StepOptions {
        temperature: Some(0.0),
        no_thinking: true,
        slot: Some(crate::server::ANSWER_SLOT),
        ..StepOptions::default()
    };
    let mut stdout = std::io::stdout();
    let turn = backend.step(&messages, &[], &options, &mut |delta| {
        if let Delta::Text(t) = delta {
            let _ = write!(stdout, "{t}");
            let _ = stdout.flush();
        }
    })?;
    println!();
    context.printed.push_str(&turn.content);
    context.printed.push('\n');
    Ok(())
}

fn pop(context: &mut Context) -> Result<Value, String> {
    context.stack.pop().ok_or_else(|| "the stack is empty".to_string())
}

/// Folders first, then files, each alphabetical; folders end with `/`.
fn list_directory(path: &std::path::Path) -> std::io::Result<Vec<String>> {
    let mut folders = vec![];
    let mut files = vec![];
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type()?.is_dir() {
            folders.push(format!("{name}/"));
        } else {
            files.push(name);
        }
    }
    folders.sort();
    files.sort();
    folders.extend(files);
    Ok(folders)
}

/// Numbered lines `start..=end` (1-based); 0 means "from the start" or "as far as allowed".
pub fn view_lines(text: &str, start: i64, end: i64) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = start.max(1) as usize;
    let limit = start + VIEW_MAX_LINES - 1;
    let end = if end <= 0 { limit } else { (end as usize).min(limit) }.min(lines.len());
    if start > lines.len() {
        return format!(
            "(start {start} is past the end; the text has {} lines, so use a start from 1 to {})",
            lines.len(),
            lines.len()
        );
    }
    bounded_numbered_lines((start..=end).map(|n| (n, lines[n - 1])), lines.len() - end)
}

/// Numbered lines containing `pattern`, ignoring case; capped like `reg_view`.
pub fn search_lines(text: &str, pattern: &str) -> String {
    let needle = pattern.to_lowercase();
    let matches: Vec<(usize, &str)> = text
        .lines()
        .enumerate()
        .filter(|(_, line)| line.to_lowercase().contains(&needle))
        .map(|(i, line)| (i + 1, line))
        .collect();
    if matches.is_empty() {
        return format!("(no lines contain {pattern:?})");
    }
    let total = matches.len();
    bounded_numbered_lines(matches.into_iter().take(VIEW_MAX_LINES), total.saturating_sub(VIEW_MAX_LINES))
}

fn bounded_numbered_lines<'a>(lines: impl Iterator<Item = (usize, &'a str)>, more: usize) -> String {
    let mut out = String::new();
    let mut omitted = more;
    let mut byte_capped = false;
    for (number, line) in lines {
        let row = format!("{number}\t{line}");
        let needed = row.len() + usize::from(!out.is_empty());
        if out.len() + needed > VIEW_MAX_BYTES {
            omitted += 1;
            byte_capped = true;
            continue;
        }
        if !out.is_empty() { out.push('\n'); }
        out.push_str(&row);
    }
    if omitted > 0 {
        let suffix = if byte_capped {
            format!("\n({omitted} more lines or matches; output capped at {VIEW_MAX_BYTES} bytes)")
        } else {
            format!("\n({omitted} more lines)")
        };
        if out.len() + suffix.len() <= VIEW_MAX_BYTES { out.push_str(&suffix); }
    }
    out
}

fn replace_once(text: &str, find: &str, replace: &str, register: usize) -> Result<String, String> {
    if find.is_empty() {
        return Err("`find` must not be empty".into());
    }
    match text.matches(find).count() {
        1 => Ok(text.replacen(find, replace, 1)),
        0 => Err(format!("`find` does not occur in R{register}")),
        n => Err(format!(
            "`find` occurs {n} times in R{register}; include more surrounding text"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_numbered_line_ranges() {
        let text = "a\nb\nc\nd";
        let cases = [
            (1, 2, "1\ta\n2\tb\n(2 more lines)"),
            (3, 0, "3\tc\n4\td"),
            (0, 1, "1\ta\n(3 more lines)"),
            (9, 10, "(start 9 is past the end; the text has 4 lines, so use a start from 1 to 4)"),
        ];
        for (start, end, expected) in cases {
            assert_eq!(view_lines(text, start, end), expected, "{start}-{end}");
        }
    }

    #[test]
    fn view_caps_the_line_count() {
        let text = (1..=500).map(|n| n.to_string()).collect::<Vec<_>>().join("\n");
        let view = view_lines(&text, 1, 0);
        assert_eq!(view.lines().count(), VIEW_MAX_LINES + 1);
        assert!(view.ends_with("(300 more lines)"));
    }

    #[test]
    fn searches_lines_ignoring_case() {
        let text = "fn a() {}\nlet M4 = 1;\nlet m4_pro = 2;";
        assert_eq!(search_lines(text, "m4"), "2\tlet M4 = 1;\n3\tlet m4_pro = 2;");
        assert_eq!(search_lines(text, "zzz"), "(no lines contain \"zzz\")");
    }

    #[test]
    fn arithmetic_is_checked() {
        let cases = [
            (Ops::Add, 1, 1, Ok(2)),
            (Ops::Subtract, 2, 5, Ok(-3)),
            (Ops::Multiply, 12, 7, Ok(84)),
            (Ops::Divide, 7, 2, Ok(3)),
            (Ops::Divide, 1, 0, Err("division by zero".to_string())),
            (Ops::Add, i64::MAX, 1, Err(format!("{} + 1 overflows", i64::MAX))),
        ];
        for (op, a, b, expected) in cases {
            assert_eq!(arithmetic(&op, a, b), expected, "{a} {} {b}", op.name());
        }
    }

    #[test]
    fn replace_requires_exactly_one_match() {
        assert_eq!(replace_once("a b c", "b", "x", 5), Ok("a x c".into()));
        assert!(replace_once("a b b", "b", "x", 5).unwrap_err().contains("2 times"));
        assert!(replace_once("a b c", "z", "x", 5).unwrap_err().contains("does not occur"));
        assert!(replace_once("a", "", "x", 5).is_err());
    }
}

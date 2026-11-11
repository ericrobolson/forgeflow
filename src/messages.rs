use std::{
    error::Error,
    io::{self, IsTerminal, Write},
};

use crossterm::{
    event::{
        self, Event as KeyInput, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode},
};

use std::time::{Duration, Instant};

use serde_json::json;

use crate::{
    chat::{self, ChatBackend, Delta, OpenAiCompatible, StepOptions},
    config::Config,
    models,
    ops::Ops,
    parser::{self, Source},
    translate::{self, Exchange},
    words,
    events::{Event, Kind},
    web::SharedConversation,
    workflow::{self, Context},
};

const CONTINUATION_INDENT: &str = "  ";

/// A turn-by-turn interface: ordinary messages go to chat; `;` prefixed messages run as code.
/// Output goes to the context's event sink, so the terminal and web UI share it.
pub struct Conversation {
    pub context: Context,
    backend: OpenAiCompatible,
    history: Vec<Exchange>,
    context_limit: u64,
    /// Text to place on the next input line, set by suggest mode.
    prefill: Option<String>,
    /// Whether full-screen commands such as `/edit` can take over the terminal.
    pub interactive: bool,
}

/// Start a conversation with the selected model.
///
/// Uses Crossterm key events for a real terminal and line input when stdin is
/// piped or redirected. Shift+Enter inserts a newline where the terminal
/// reports that key combination; a trailing backslash is a portable fallback.
pub fn run(context: Context) -> Result<(), Box<dyn Error>> {
    let terminal = io::stdin().is_terminal();
    // Print the banner before the server starts, so early keystrokes land after it.
    if terminal {
        println!(
            "ForgeFlow. Enter sends; Left/Right move the cursor, Option+Left/Right move by word; Up/Down browse history; Esc clears the line; prefix a program with ; to compile and run it. Shift+Enter adds a line where supported; Ctrl+C exits."
        );
    } else {
        println!("ForgeFlow. Send a message; prefix a program with ; to compile and run it; Ctrl+D to exit.");
    }
    let conversation = crate::web::share(context, terminal, false)?;
    if terminal {
        run_terminal(&conversation)
    } else {
        run_stream(&conversation)
    }
}

fn report_submission(
    buffer: &mut String,
    _submission: usize,
    terminal_mode: bool,
    conversation: &SharedConversation,
) -> Result<(), Box<dyn Error>> {
    let message = std::mem::take(buffer);
    // Crossterm raw mode disables the terminal's normal LF-to-CRLF output
    // handling. Run the turn in cooked mode, then resume raw key input.
    if terminal_mode {
        disable_raw_mode()?;
    }
    if !message.trim().is_empty() {
        // Held only for the turn, so the web UI can take turns while this one waits for input.
        conversation.lock().expect("conversation lock").turn(message.trim());
    }
    if terminal_mode {
        enable_raw_mode()?;
    }
    Ok(())
}

impl Conversation {
    /// Connects to the selected model, starting its server now so the first message does
    /// not wait for it.
    pub fn new(mut context: Context, interactive: bool) -> Result<Self, String> {
        let (backend, config) = chat::connect(&mut context)?;
        Ok(Self {
            context,
            backend,
            history: vec![],
            context_limit: config.ctx,
            prefill: None,
            interactive,
        })
    }

    /// The word listing about `focus` at the most detailed level that, with `fixed_chars` of
    /// instructions around it, fits in `budget` tokens; small contexts get shorter listings.
    fn fitted_listing(&self, focus: &str, fixed_chars: usize, budget: u64) -> String {
        let words = &self.context.user_words;
        for detail in [translate::Detail::Focused, translate::Detail::Compact] {
            let listing = translate::word_listing_at(words, focus, detail);
            if ((listing.chars().count() + fixed_chars + focus.chars().count()) as u64).div_ceil(3) <= budget {
                return listing;
            }
        }
        translate::word_listing_at(words, focus, translate::Detail::Minimal)
    }

    fn line(&mut self, kind: Kind, text: impl Into<String>) {
        self.context.emit(Event::Line(kind, text.into()));
    }

    fn note(&mut self, text: impl Into<String>) {
        self.line(Kind::Note, text);
    }

    fn error(&mut self, text: impl std::fmt::Display) {
        self.line(Kind::Error, text.to_string());
    }

    /// Chat commands such as `/model`, handled directly and never sent to the model.
    fn command(&mut self, message: &str) {
        let words: Vec<&str> = message.split_whitespace().collect();
        let result = match words.as_slice() {
            // The rest of the line is the path, so paths may contain spaces.
            ["/edit", ..] if !self.interactive => Err("/edit needs the terminal; it is not available here".into()),
            ["/edit", _, ..] => crate::editor::run(&mut self.context, message.trim_start_matches("/edit")),
            ["/edit"] => Err("usage: /edit <path>".into()),
            ["/clear"] => {
                self.history.clear();
                self.context.stack.clear();
                let text = format!("Conversation and stack cleared. Estimated context remaining: ~{} tokens.", self.context_limit);
                self.line(Kind::Command, text);
                Ok(())
            }
            ["/reload"] => match crate::user_words::reload(&mut self.context) {
                Ok(count) => {
                    self.line(Kind::Command, format!("Reloaded {count} user-defined word(s)."));
                    Ok(())
                }
                Err(error) => Err(format!("reload failed; keeping current words: {error}")),
            },
            ["/model"] => {
                let current = Config::load(&self.context.project.folder())
                    .ok()
                    .and_then(|c| c.model)
                    .unwrap_or_else(|| "(none)".into());
                self.line(Kind::Command, format!("Current model: {current}\nInstalled:"));
                for line in models::installed_summary() {
                    self.line(Kind::Command, format!("  {line}"));
                }
                self.line(Kind::Command, "Switch with /model <id>; see every model with /models.");
                Ok(())
            }
            ["/model", id] => self.switch_model(id),
            ["/models"] => workflow::run_steps(&mut self.context, &[Ops::Models]),
            ["/pull", id] => models::pull(id, &mut io::stderr()).map(|_| ()),
            ["/image"] => workflow::run_steps(&mut self.context, &[Ops::ImageModels]),
            ["/image", id] => workflow::run_steps(&mut self.context, &[Ops::StringLiteral(id.to_string()), Ops::UseImage]),
            ["/serve"] => workflow::run_steps(&mut self.context, &[Ops::StringLiteral("start".into()), Ops::Serve]),
            ["/serve", action] => workflow::run_steps(&mut self.context, &[Ops::StringLiteral(action.to_string()), Ops::Serve]),
            _ => Err(format!(
                "unknown command `{message}`; try /edit <path>, /clear, /reload, /model, /model <id>, /models, /image, /image <id>, /pull <id>, /serve, /serve stop, or /help"
            )),
        };
        if let Err(error) = result {
            self.error(error);
        }
    }

    /// Selects a model, restarts the server with it, and warms its prompt cache. If the new
    /// model cannot start, the previous one is selected and started again.
    fn switch_model(&mut self, id: &str) -> Result<(), String> {
        let started = Instant::now();
        let folder = self.context.project.folder();
        let previous = Config::load(&folder)?;
        let switched = workflow::run_steps(&mut self.context, &[Ops::StringLiteral(id.to_string()), Ops::UseModel])
            .and_then(|()| chat::connect(&mut self.context))
            .map(|(backend, _)| backend);
        match switched {
            Ok(backend) => {
                self.backend = backend;
                self.note(format!("switched to {id} in {} ms", started.elapsed().as_millis()));
                Ok(())
            }
            Err(error) => {
                previous.save(&folder)?;
                self.context.server = None;
                let (backend, _) = chat::connect(&mut self.context)?;
                self.backend = backend;
                let kept = previous.model.as_deref().unwrap_or("the previous model");
                Err(format!("could not switch to {id} ({error}); still using {kept}"))
            }
        }
    }

    /// Translates one message, runs the program, and records the turn.
    pub fn turn(&mut self, message: &str) {
        // Reconnect if the server went away, such as a shared one its owner restarted.
        match chat::connect(&mut self.context) {
            Ok((backend, _)) => self.backend = backend,
            Err(error) => self.error(format!("could not reach the model: {error}")),
        }
        if message.starts_with('/') && message != "/help" {
            self.context.say_kind = Kind::Command;
            self.command(message);
            self.context.say_kind = Kind::Run;
            return;
        }
        if let Some(request) = message.trim_start().strip_prefix(';') {
            return self.run_program_request(request);
        }
        if let Some(goal) = message.trim_start().strip_prefix('?').map(str::trim).filter(|goal| !goal.is_empty()) {
            return self.suggest(goal);
        }
        self.chat(message);
    }

    /// Suggest mode: streams candidate commands for a goal. Nothing is run or recorded.
    fn suggest(&mut self, goal: &str) {
        let mut messages = translate::suggest_messages_with(goal, &self.context.user_words, self.context_limit, SUGGEST_MAX_TOKENS as u64 + 600);
        let mut valid: Vec<String> = Vec::new();
        let mut notes: Vec<String> = Vec::new();
        for attempt in 0..3 {
            // Retries carry earlier replies; stop before the prompt outgrows the context.
            if estimate_tokens(&messages) + SUGGEST_MAX_TOKENS as u64 > self.context_limit {
                if attempt == 0 {
                    return self.error(format!("that request is too long for the model's {}-token context", self.context_limit));
                }
                break;
            }
            let mut generated = String::new();
            let result = self.backend.step(
                &messages,
                &[],
                // Suggestions are a few short lines; the cap keeps a rambling reply from
                // overflowing the context when it is sent back for a retry.
                &StepOptions { slot: Some(crate::server::ANSWER_SLOT), temperature: Some(0.0), no_thinking: true, max_tokens: Some(SUGGEST_MAX_TOKENS), ..StepOptions::default() },
                &mut |delta| {
                    if let Delta::Text(text) = delta {
                        generated.push_str(&text);
                    }
                },
            );
            if let Err(error) = result {
                return self.error(error);
            }
            let mut errors = Vec::new();
            // Suggestions are steps in order, so later lines may call words defined earlier.
            let mut words = self.context.user_words.clone();
            for line in generated.lines().map(|line| line.trim().trim_matches('`').trim()).filter(|line| !line.is_empty()) {
                let Some(program) = line.strip_prefix(';').map(str::trim) else {
                    notes.push(line.to_string());
                    continue;
                };
                if program.starts_with(':') {
                    match crate::user_words::compile_one(&words, "<suggested definition>", program) {
                        Ok(defined) => {
                            words = defined;
                            valid.push(format!("; {program}"));
                        }
                        Err(error) => errors.push(format!("`; {program}`: {error}")),
                    }
                    continue;
                }
                let filled = |r: usize| self.context.registers.is_filled(r);
                match translate::compile_with(program, &filled, &words) {
                    Ok(_) => valid.push(format!("; {program}")),
                    Err(error) => errors.push(format!("`; {program}`: {error}")),
                }
            }
            if !valid.is_empty() || !notes.is_empty() || errors.is_empty() || attempt == 2 {
                for error in errors {
                    self.note(format!("rejected: {error}"));
                }
                break;
            }
            messages.push(json!({"role": "assistant", "content": generated}));
            let definition_hint = if errors.iter().any(|e| e.contains("`; :")) {
                " Every parameter in a definition needs a type after its name, written `( path String -- result String )` (types: String, Int, Bool, List, Register, Address, Any); the part after `--` lists outputs the same way and is empty for none. Inputs are already on the stack, so the body does not name them. `print` and `answer` leave nothing on the stack, so a body ending in either declares no outputs. Do not put quoted text such as \"previous output\" or \"path\" in the body unless you mean a literal string."
            } else {
                ""
            };
            messages.push(json!({"role": "user", "content": format!(
                "Those failed to compile: {}. Only the listed words exist, and each word takes its inputs from the stack in the order shown (for example `\"out.png\" \"prompt\" make-image`).{definition_hint} Reply with corrected `; ` commands only.",
                errors.join("; ")
            )}));
        }
        valid.dedup();
        // The prompt takes one line: the steps joined when they run as one program, or else
        // the first suggestion.
        let filled = |r: usize| self.context.registers.is_filled(r);
        let joined = join_steps(&valid, &self.context.user_words, &filled);
        match &joined {
            Some(line) => self.line(Kind::Suggest, line.clone()),
            None => {
                for command in &valid {
                    self.line(Kind::Suggest, command.clone());
                }
            }
        }
        self.prefill = joined.or_else(|| valid.first().cloned());
        for note in &notes {
            self.line(Kind::Chat, note.clone());
        }
        if valid.is_empty() && notes.is_empty() {
            self.note("no valid command suggestions");
        }
    }

    /// Sends ordinary messages directly to the model with no tools available.
    fn chat(&mut self, message: &str) {
        // The instructions get at most half of what the reply leaves; history uses the rest.
        let words = self.fitted_listing(message, CHAT_INSTRUCTION_CHARS, self.context_limit.saturating_sub(CHAT_REPLY_TOKENS) / 2);
        let mut messages = vec![json!({
            "role": "system",
            "content": format!("{}\n\nThese are all the words that exist, written `word ( inputs -- outputs )`. Registers are written R0 to R15. Use only these words in any ForgeFlow example; never invent words, and never use `define`, `end`, `if`, loops other than `[ ... ] each`, or other languages' syntax. Words cannot write files, run commands or branch, so say so plainly if the user asks for that. Project words are defined with `: name ( inputName Type -- outputName Type ) body ;` (for example `: square ( n Int -- result Int ) dup * ;`), or with `; define a function called NAME that DESCRIPTION`, which writes the definition to `_forgeflow/user_words.ff.txt` for the user.\n\n{}", "You are ForgeFlow, a helpful conversational assistant. Answer the user's message directly. Include a concrete, copyable example only when the user is clearly asking for instructions on how to do something; don't add an example to general questions or direct requests unless requested. In translator mode (messages prefixed with `;`), the translator handles those how-to requests. Keep any ForgeFlow example valid: it is a postfix stack language, and runnable programs are prefixed with a semicolon. For example, a durable variable can be declared by adding `variable prompt` to `_forgeflow/user_words.ff.txt`. After it is loaded, store a prompt with `; \"A watercolor fox\" prompt !` and generate an image with `; \"art/fox.png\" prompt @ make-image` (the output path comes first, then the prompt). `$prompt` refers to a named input inside a user-defined word, not to a durable variable. No tools are available in chat. To run ForgeFlow code, the user starts a message with a semicolon.", words)
        })];
        // Leave room for the reply: drop the oldest turns until the prompt fits.
        let budget = self.context_limit.saturating_sub(CHAT_REPLY_TOKENS);
        let fixed = estimate_tokens(&messages) + estimate_tokens(&[json!({"role": "user", "content": message})]);
        let mut kept = self.history.len();
        while kept > 0 && fixed + history_tokens(&self.history[self.history.len() - kept..]) > budget {
            kept -= 1;
        }
        if kept < self.history.len() {
            self.note(format!("(left out the {} oldest turn(s) to fit the model's {}-token context)", self.history.len() - kept, self.context_limit));
        }
        for exchange in &self.history[self.history.len() - kept..] {
            messages.push(json!({"role": "user", "content": exchange.message}));
            messages.push(json!({"role": "assistant", "content": exchange.output}));
        }
        messages.push(json!({"role": "user", "content": message}));
        if estimate_tokens(&messages) > budget {
            return self.error(format!("that message is too long for the model's {}-token context; shorten it or raise `ctx` in _forgeflow/config.json", self.context_limit));
        }

        let mut output = String::new();
        let sink = &mut self.context.sink;
        let turn = self.backend.step(
            &messages,
            &[],
            &StepOptions {
                slot: Some(crate::server::ANSWER_SLOT),
                ..StepOptions::default()
            },
            &mut |delta| {
                if let Delta::Text(text) = delta {
                    output.push_str(&text);
                    sink.emit(Event::Text(Kind::Chat, text));
                }
            },
        );
        match turn {
            Ok(turn) => {
                let assistant_chars = turn.content.len();
                if !output.is_empty() {
                    self.context.emit(Event::Text(Kind::Chat, "\n".into()));
                }
                if !turn.tool_calls.is_empty() {
                    self.error("chat does not run tools; prefix ForgeFlow code with `;` to execute it");
                }
                self.history.push(Exchange {
                    message: message.to_string(),
                    program: String::new(),
                    output: turn.content,
                });
                let prompt_chars: usize = messages.iter().map(|m| m.to_string().len()).sum::<usize>()
                    + assistant_chars;
                let estimated_used = (prompt_chars as u64).div_ceil(4);
                let remaining = self.context_limit.saturating_sub(estimated_used);
                self.note(format!("~{remaining} context tokens remaining (estimated)"));
            }
            Err(error) => self.error(error),
        }
    }

    /// Translates a semicolon-prefixed request to a checked ForgeFlow program and runs it.
    fn run_program_request(&mut self, request: &str) {
        if request.trim_start().starts_with(':') {
            // A definition may be followed by code that uses it, on the same line.
            let (definition, rest) = split_definition(request.trim());
            if self.save_definition(definition) && !rest.is_empty() {
                self.run_program_request(rest);
            }
            return;
        }
        if is_definition_request(request) {
            return self.initialize_word(request);
        }
        let started = Instant::now();
        let (program, steps, attempts) = match compile_direct_program(request, &self.context.user_words, self.context.stack_types()) {
            Ok(Some(steps)) => (request.trim().to_string(), steps, 0),
            Err(error) => return self.error(error),
            Ok(None) => {
                let translated = match translate::translate_on_stack(
                    &self.backend,
                    &self.history,
                    request.trim(),
                    &self.context.registers,
                    &self.context.user_words,
                    &self.context.stack,
                    self.context_limit,
                ) {
                    Ok(translated) => translated,
                    Err(error) => return self.error(error),
                };
                (translated.program, translated.steps, translated.attempts)
            }
        };
        let translated_in = started.elapsed();
        self.note(format!("→ {program}"));

        let running = Instant::now();
        self.context.take_printed();
        let stack_before = self.context.stack.clone();
        self.context.store_label = program.clone();
        if let Err(error) = workflow::run_steps(&mut self.context, &steps) {
            self.context.stack = stack_before;
            self.error(&error);
            self.context.printed.push_str(&format!("error: {error}\n"));
        }
        if !self.context.stack.is_empty() {
            let stack = format!("stack: {}", translate::stack_summary(&self.context.stack));
            self.line(Kind::Stack, stack);
        }
        self.note(timing(translated_in, running.elapsed(), attempts));
        self.history.push(Exchange {
            message: format!(";{}", request.trim()),
            program,
            output: self.context.take_printed(),
        });
    }

    /// Checks a written `: name ( ... ) body ;` definition and saves it to the word file.
    /// Returns whether it was saved.
    fn save_definition(&mut self, definition: &str) -> bool {
        match crate::user_words::compile_one(&self.context.user_words, "<definition>", definition)
            .and_then(|_| crate::user_words::add_definition(&self.context.project, definition))
        {
            Ok(words) => {
                let name = definition.split_whitespace().nth(1).unwrap_or("word").to_string();
                self.context.user_words = words;
                self.line(Kind::Run, format!("Defined `{name}`. Call it in a program, for example `; \"x\" {name}`."));
                true
            }
            Err(error) => {
                self.error(error);
                false
            }
        }
    }

    /// Turns a natural-language definition request into one checked, persistent user word.
    fn initialize_word(&mut self, request: &str) {
        let words = self.fitted_listing(request, DEFINE_INSTRUCTION_CHARS, self.context_limit.saturating_sub(DEFINE_MAX_TOKENS as u64 * 3));
        let mut messages = vec![json!({
            "role": "system",
            "content": format!("{}\n\nThese are the only words the body may call, written `word ( inputs -- outputs )`; nothing else exists (there is no if, begin, open, type, 2dup, write_file or similar). Registers R0 to R15 are written as-is. The definition must end with `;`.\n{}", "You are the ForgeFlow word definition initializer. Convert the user's request into exactly one ForgeFlow definition, with no explanation, markdown, or surrounding text. Exact syntax: : name ( inputName Type -- outputName Type ) body ;. Example: : square ( n Int -- result Int ) dup * ;. Inputs are already on the initial data stack, so use stack operations such as dup; do not put words like 'input' in the code. Named inputs may be copied with $inputName. The body must leave exactly the declared outputs. Supported types: String, Int, Bool, List, Register, Address, Any. If the user does not specify an input type and the operation is type-generic (for example dup/drop/swap/print), declare the input as Any rather than guessing String or Int. ForgeFlow is a postfix stack language; use only its normal operators and currently available words. Never invent syntax from another language.", words)
        })];
        messages.push(json!({"role":"user", "content":request.trim()}));
        let mut last_error = String::new();
        for attempt in 0..3 {
            let mut generated = String::new();
            let response = self.backend.step(
                &messages,
                &[],
                &StepOptions { temperature: Some(0.0), max_tokens: Some(DEFINE_MAX_TOKENS), no_thinking: true, ..StepOptions::default() },
                &mut |delta| if let Delta::Text(text) = delta { generated.push_str(&text); },
            );
            if let Err(error) = response { return self.error(format!("could not initialize word: {error}")); }
            let definition = unwrap_definition(&generated);
            match crate::user_words::compile_one(&self.context.user_words, "<generated definition>", definition) {
                Ok(_) => match crate::user_words::add_definition(&self.context.project, definition) {
                    Ok(words) => {
                        let name = definition.split_whitespace().nth(1).unwrap_or("word").to_string();
                        self.context.user_words = words;
                        self.line(Kind::Run, format!("Defined `{name}`. Call it in a program, for example `; 3 {name}`."));
                        self.note(format!("→ {}", definition.trim()));
                        self.history.push(Exchange { message: format!(";{}", request.trim()), program: definition.trim().to_string(), output: format!("Defined `{name}`.") });
                        return;
                    }
                    Err(error) => last_error = error,
                },
                Err(error) => last_error = error,
            }
            if attempt < 2 {
                messages.push(json!({"role":"assistant", "content":generated}));
                messages.push(json!({"role":"user", "content":format!("That definition failed validation: {last_error}. Return one corrected complete ForgeFlow definition only. Follow the exact syntax and square example from the system instructions.")}));
            }
        }
        self.error(format!("could not initialize word after 3 attempts: {last_error}"));
    }
}

/// Splits `: name ( ... ) body ; more code` after the definition's closing `;`: the first
/// `;` standing alone as a word outside a string.
fn split_definition(text: &str) -> (&str, &str) {
    let mut in_string = false;
    let mut escaped = false;
    let mut previous = ' ';
    for (at, c) in text.char_indices() {
        if in_string {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
        } else if c == '"' {
            in_string = true;
        } else if c == ';' && previous.is_whitespace() {
            let end = at + 1;
            if text[end..].chars().next().is_none_or(char::is_whitespace) {
                return (&text[..end], text[end..].trim());
            }
        }
        previous = c;
    }
    (text, "")
}

/// Suggested steps (`; ...` lines, definitions first) as one `; ...` line, when there are
/// several and together they compile as one program.
fn join_steps(steps: &[String], user_words: &[crate::user_words::UserWord], filled: &dyn Fn(usize) -> bool) -> Option<String> {
    if steps.len() < 2 {
        return None;
    }
    let mut words = user_words.to_vec();
    let mut code: Vec<&str> = vec![];
    for step in steps.iter().map(|s| s.trim_start_matches(';').trim()) {
        if step.starts_with(':') {
            // A definition after code would run before it on one line, changing the order.
            if !code.is_empty() {
                return None;
            }
            words = crate::user_words::compile_one(&words, "<suggested definition>", step).ok()?;
        } else {
            code.push(step);
        }
    }
    let program = code.join(" ");
    if !program.is_empty() {
        translate::compile_with(&program, filled, &words).ok()?;
    }
    let all: Vec<&str> = steps.iter().map(|s| s.trim_start_matches(';').trim()).collect();
    Some(format!("; {}", all.join(" ")))
}

/// Most tokens a suggestion reply may use.
const SUGGEST_MAX_TOKENS: u32 = 400;
/// Tokens kept free for a chat reply.
const CHAT_REPLY_TOKENS: u64 = 1500;
/// Most tokens a generated word definition may use.
const DEFINE_MAX_TOKENS: u32 = 400;
/// The length of the chat and word-definition instructions around the word listing.
const CHAT_INSTRUCTION_CHARS: usize = 2400;
const DEFINE_INSTRUCTION_CHARS: usize = 1300;

use translate::estimate_tokens;

fn history_tokens(history: &[Exchange]) -> u64 {
    history.iter().map(|e| (e.message.chars().count() + e.output.chars().count() + 40) as u64).sum::<u64>().div_ceil(3)
}

fn is_definition_request(request: &str) -> bool {
    let request = request.trim_start().to_ascii_lowercase();
    ["define ", "define\n", "define\t", "i want you to define ", "please define ", "can you define ", "could you define ", "help me define "]
        .iter().any(|prefix| request.starts_with(prefix))
}

fn unwrap_definition(generated: &str) -> &str {
    let text = generated.trim();
    if let Some(rest) = text.strip_prefix("```") {
        let rest = rest.strip_prefix("forth").or_else(|| rest.strip_prefix("ff")).unwrap_or(rest);
        return rest.trim().strip_suffix("```").unwrap_or(rest.trim()).trim();
    }
    text
}

/// Uses valid source after `;` as code directly; natural language falls through to translation.
fn compile_direct_program(source_text: &str, user_words: &[crate::user_words::UserWord], stack: Vec<crate::type_::TypeKind>) -> Result<Option<Vec<Ops>>, String> {
    let source = Source::new("<code>", source_text);
    let Ok(parsed) = parser::parse(&source) else { return Ok(None); };
    let mentions_user_word = parsed.tokens.iter().any(|token| match &token.kind {
        parser::TokenKind::Identifier(name) => user_words.iter().any(|word| word.name == *name),
        _ => false,
    });
    let descriptors: std::collections::HashMap<String, Ops> = user_words.iter().map(|w| (w.name.clone(), Ops::UserCall {
        name: w.name.clone(), inputs: w.inputs.iter().map(|p| p.kind).collect(), outputs: w.outputs.iter().map(|p| p.kind).collect(),
    })).collect();
    let steps = match words::compile_with(&parsed.tokens, &descriptors, &std::collections::HashMap::new()) {
        Ok(steps) => steps,
        Err(errors) if mentions_user_word => return Err(errors.iter().map(|e| e.message.as_str()).collect::<Vec<_>>().join("; ")),
        Err(_) => return Ok(None),
    };
    if let Err(error) = workflow::check_steps(stack, &steps) {
        return if mentions_user_word { Err(error) } else { Ok(None) };
    }
    Ok(Some(steps))
}

fn timing(translated: Duration, ran: Duration, attempts: usize) -> String {
    if attempts == 0 {
        return format!("compile {} ms · run {} ms", translated.as_millis(), ran.as_millis());
    }
    let retried = if attempts > 1 { " (retried)" } else { "" };
    format!("translate {} ms{retried} · run {} ms", translated.as_millis(), ran.as_millis())
}

fn accept_line(buffer: &mut String, line: &str) -> bool {
    buffer.push_str(line);
    if buffer.ends_with('\\') {
        buffer.pop();
        buffer.push('\n');
        true
    } else {
        false
    }
}

fn prompt(buffer_is_empty: bool) -> &'static str {
    if buffer_is_empty { "> " } else { "" }
}

#[cfg(test)]
mod definition_request_tests {
    use super::*;

    #[test]
    fn splits_a_definition_from_the_code_after_it() {
        assert_eq!(
            split_definition(": double ( n Int -- result Int ) dup + ; 4 double print"),
            (": double ( n Int -- result Int ) dup + ;", "4 double print")
        );
        assert_eq!(
            split_definition(r#": say ( -- ) "a ; b \" ;" print ;"#),
            (r#": say ( -- ) "a ; b \" ;" print ;"#, "")
        );
    }

    #[test]
    fn joins_suggested_steps_into_one_line() {
        let none = |_: usize| false;
        let steps = ["; : double ( n Int -- result Int ) dup + ;".to_string(), "; 4 double print".to_string()];
        assert_eq!(
            join_steps(&steps, &[], &none).as_deref(),
            Some("; : double ( n Int -- result Int ) dup + ; 4 double print")
        );
        let program = [r#"; "a" R4 !"#.to_string(), "; R4 @ print".to_string()];
        assert_eq!(join_steps(&program, &[], &none).as_deref(), Some(r#"; "a" R4 ! R4 @ print"#));
        assert_eq!(join_steps(&steps[..1], &[], &none), None);
        // Steps that do not run together keep separate lines.
        assert_eq!(join_steps(&["; 4 nosuchword".to_string(), "; 1 print".to_string()], &[], &none), None);
    }

    #[test]
    fn recognizes_natural_definition_requests_case_insensitively() {
        assert!(is_definition_request("define a function called square"));
        assert!(is_definition_request("  DEFINE\tword foo"));
        assert!(is_definition_request("i want you to define a function called square"));
        assert!(is_definition_request("Please define square as dup multiply"));
        assert!(!is_definition_request("definition of square"));
        assert!(!is_definition_request("square 3"));
        assert!(!is_definition_request(""));
    }

    #[test]
    fn removes_only_a_single_surrounding_fence() {
        assert_eq!(unwrap_definition(": square ( n Int -- result Int ) dup * ;"), ": square ( n Int -- result Int ) dup * ;");
        assert_eq!(unwrap_definition("```forth\n: square ( n Int -- result Int ) dup * ;\n```"), ": square ( n Int -- result Int ) dup * ;");
        assert_eq!(unwrap_definition("```ff\n: square ( n Int -- result Int ) dup * ;\n```"), ": square ( n Int -- result Int ) dup * ;");
    }

    #[test]
    fn direct_user_word_calls_keep_type_and_stack_errors() {
        let words = crate::user_words::compile_one(
            &[],
            "test.ff",
            ": printTwice ( value Any -- ) dup print print ;",
        ).unwrap();
        assert!(compile_direct_program("3 printTwice", &words, vec![]).unwrap().is_some());
        assert!(compile_direct_program("square", &words, vec![]).unwrap().is_none());
        assert!(compile_direct_program("printTwice", &words, vec![]).is_err());
        // Values left by earlier programs are on the stack for the next one.
        assert!(compile_direct_program("dup", &words, vec![]).unwrap().is_none());
        assert!(compile_direct_program("dup", &words, vec![crate::type_::TypeKind::Int]).unwrap().is_some());
    }
}

fn is_multiline_enter(key: KeyEvent) -> bool {
    (key.code == KeyCode::Enter
        && key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT))
        // Ctrl+J is a portable newline chord in raw terminal input. Some
        // terminals encode Shift+Enter as Alt+Enter (Escape followed by CR).
        || (key.code == KeyCode::Char('j') && key.modifiers.contains(KeyModifiers::CONTROL))
}

fn begin_continuation(buffer: &mut String, current_line: &mut String) {
    buffer.push_str(current_line);
    buffer.push('\n');
    current_line.clear();
    current_line.push_str(CONTINUATION_INDENT);
}

fn char_byte_index(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map_or(text.len(), |(byte_index, _)| byte_index)
}

fn insert_at_cursor(line: &mut String, cursor: &mut usize, text: &str) {
    line.insert_str(char_byte_index(line, *cursor), text);
    *cursor += text.chars().count();
}

fn move_left_word(line: &str, cursor: usize) -> usize {
    let chars: Vec<char> = line.chars().collect();
    let mut position = cursor.min(chars.len());
    while position > 0 && chars[position - 1].is_whitespace() {
        position -= 1;
    }
    while position > 0 && !chars[position - 1].is_whitespace() {
        position -= 1;
    }
    position
}

fn move_right_word(line: &str, cursor: usize) -> usize {
    let chars: Vec<char> = line.chars().collect();
    let mut position = cursor.min(chars.len());
    while position < chars.len() && !chars[position].is_whitespace() {
        position += 1;
    }
    while position < chars.len() && chars[position].is_whitespace() {
        position += 1;
    }
    position
}

/// Where the input being edited sits on screen, in rows below its first row, so a line
/// longer than the terminal is wide can be redrawn in place.
#[derive(Default)]
struct InputRows {
    cursor: usize,
    end: usize,
}

impl InputRows {
    /// Moves below the input to a fresh line, as when it is submitted.
    fn leave(&mut self) {
        if self.end > self.cursor {
            print!("\x1b[{}B", self.end - self.cursor);
        }
        print!("\r\n");
        *self = Self::default();
    }
}

/// For `columns` characters of prompt and line in a terminal `width` wide, with the cursor
/// after `position` of them: the row the text ends on, and the cursor's row and column.
/// A line that exactly fills its last row ends at the start of the next, where the cursor
/// is placed so the terminal's pending wrap never leaves it ambiguous.
fn input_layout(width: usize, columns: usize, position: usize) -> (usize, usize, usize) {
    let width = width.max(1);
    (columns / width, position / width, position % width)
}

fn redraw_input(stdout: &mut impl Write, rows: &mut InputRows, buffer_is_empty: bool, line: &str, cursor: usize) -> io::Result<()> {
    let prompt = prompt(buffer_is_empty);
    let width = crossterm::terminal::size().map_or(80, |(columns, _)| columns as usize);
    // Back to the input's first row, then clear everything below before drawing it again.
    if rows.cursor > 0 {
        print!("\x1b[{}A", rows.cursor);
    }
    print!("\r\x1b[J{prompt}{line}");
    let columns = prompt.chars().count() + line.chars().count();
    let (end, row, column) = input_layout(width, columns, prompt.chars().count() + cursor);
    if columns > 0 && columns % width.max(1) == 0 {
        print!("\r\n");
    }
    if end > row {
        print!("\x1b[{}A", end - row);
    }
    print!("\r");
    if column > 0 {
        print!("\x1b[{column}C");
    }
    *rows = InputRows { cursor: row, end };
    stdout.flush()
}

fn run_stream(conversation: &SharedConversation) -> Result<(), Box<dyn Error>> {
    let stdin = io::stdin();
    let mut buffer = String::new();
    let mut line = String::new();
    let mut submission = 1;
    loop {
        print!("{}", prompt(buffer.is_empty()));
        io::stdout().flush()?;
        line.clear();
        if stdin.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end_matches(|ch| ch == '\r' || ch == '\n');
        if accept_line(&mut buffer, line) {
            continue;
        }
        report_submission(&mut buffer, submission, false, conversation)?;
        submission += 1;
    }
    Ok(())
}

struct RawMode;

impl RawMode {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

struct KeyboardEnhancement;

impl KeyboardEnhancement {
    fn try_enable() -> Option<Self> {
        let mut stdout = io::stdout();
        execute!(
            stdout,
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES,
            )
        )
        .ok()?;
        Some(Self)
    }
}

impl Drop for KeyboardEnhancement {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
    }
}

fn run_terminal(conversation: &SharedConversation) -> Result<(), Box<dyn Error>> {
    let _raw_mode = RawMode::enter()?;
    // Request unambiguous reports for modified keys, including Shift+Enter.
    // Other terminals ignore the request and retain the trailing-backslash
    // continuation fallback.
    let _keyboard_enhancement = KeyboardEnhancement::try_enable();
    let mut stdout = io::stdout();
    let mut buffer = String::new();
    let mut current_line = String::new();
    let mut cursor = 0usize;
    let mut input_history: Vec<String> = conversation
        .lock()
        .expect("conversation lock")
        .history
        .iter()
        .map(|exchange| exchange.message.clone())
        .filter(|message| !message.contains('\n'))
        .collect();
    let mut history_position: Option<usize> = None;
    let mut history_draft = String::new();
    let mut submission = 1;
    let mut rows = InputRows::default();
    print!("> ");
    stdout.flush()?;

    loop {
        match event::read()? {
            KeyInput::Key(key) if key.kind == KeyEventKind::Press => {
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    rows.leave();
                    stdout.flush()?;
                    break;
                }
                if is_multiline_enter(key) {
                    begin_continuation(&mut buffer, &mut current_line);
                    cursor = current_line.chars().count();
                    rows.leave();
                    redraw_input(&mut stdout, &mut rows, false, &current_line, cursor)?;
                    continue;
                }
                match key.code {
                    KeyCode::Enter => {
                        if current_line.ends_with('\\') {
                            current_line.pop();
                            begin_continuation(&mut buffer, &mut current_line);
                            cursor = current_line.chars().count();
                            rows.leave();
                            redraw_input(&mut stdout, &mut rows, false, &current_line, cursor)?;
                        } else {
                            buffer.push_str(&current_line);
                            let submitted = buffer.clone();
                            rows.leave();
                            report_submission(&mut buffer, submission, true, conversation)?;
                            if !submitted.trim().is_empty()
                                && !submitted.contains('\n')
                                && input_history.last() != Some(&submitted)
                            {
                                input_history.push(submitted);
                            }
                            history_position = None;
                            history_draft.clear();
                            submission += 1;
                            current_line = conversation.lock().expect("conversation lock").prefill.take().unwrap_or_default();
                            cursor = current_line.chars().count();
                            redraw_input(&mut stdout, &mut rows, true, &current_line, cursor)?;
                        }
                    }
                    KeyCode::Up if buffer.is_empty() && !input_history.is_empty() => {
                        let position = match history_position {
                            Some(position) => position.saturating_sub(1),
                            None => {
                                history_draft.clone_from(&current_line);
                                input_history.len() - 1
                            }
                        };
                        history_position = Some(position);
                        current_line.clone_from(&input_history[position]);
                        cursor = current_line.chars().count();
                        redraw_input(&mut stdout, &mut rows, true, &current_line, cursor)?;
                    }
                    KeyCode::Down if buffer.is_empty() => {
                        if let Some(position) = history_position {
                            if position + 1 < input_history.len() {
                                history_position = Some(position + 1);
                                current_line.clone_from(&input_history[position + 1]);
                            } else {
                                history_position = None;
                                current_line.clone_from(&history_draft);
                            }
                            cursor = current_line.chars().count();
                            redraw_input(&mut stdout, &mut rows, true, &current_line, cursor)?;
                        }
                    }
                    KeyCode::Backspace => {
                        if cursor > 0 {
                            let start = char_byte_index(&current_line, cursor - 1);
                            let end = char_byte_index(&current_line, cursor);
                            current_line.replace_range(start..end, "");
                            cursor -= 1;
                        }
                        history_position = None;
                        redraw_input(&mut stdout, &mut rows, buffer.is_empty(), &current_line, cursor)?;
                    }
                    KeyCode::Delete => {
                        let end = char_byte_index(&current_line, cursor + 1);
                        let start = char_byte_index(&current_line, cursor);
                        if end > start {
                            current_line.replace_range(start..end, "");
                        }
                        history_position = None;
                        redraw_input(&mut stdout, &mut rows, buffer.is_empty(), &current_line, cursor)?;
                    }
                    KeyCode::Left if key.modifiers.intersects(KeyModifiers::META | KeyModifiers::SUPER) => {
                        cursor = move_left_word(&current_line, cursor);
                        redraw_input(&mut stdout, &mut rows, buffer.is_empty(), &current_line, cursor)?;
                    }
                    KeyCode::Left if cursor > 0 => {
                        cursor -= 1;
                        redraw_input(&mut stdout, &mut rows, buffer.is_empty(), &current_line, cursor)?;
                    }
                    KeyCode::Right if key.modifiers.intersects(KeyModifiers::META | KeyModifiers::SUPER) => {
                        cursor = move_right_word(&current_line, cursor);
                        redraw_input(&mut stdout, &mut rows, buffer.is_empty(), &current_line, cursor)?;
                    }
                    KeyCode::Right if cursor < current_line.chars().count() => {
                        cursor += 1;
                        redraw_input(&mut stdout, &mut rows, buffer.is_empty(), &current_line, cursor)?;
                    }
                    KeyCode::Home => {
                        cursor = 0;
                        redraw_input(&mut stdout, &mut rows, buffer.is_empty(), &current_line, cursor)?;
                    }
                    KeyCode::End => {
                        cursor = current_line.chars().count();
                        redraw_input(&mut stdout, &mut rows, buffer.is_empty(), &current_line, cursor)?;
                    }
                    KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        insert_at_cursor(&mut current_line, &mut cursor, &ch.to_string());
                        history_position = None;
                        redraw_input(&mut stdout, &mut rows, buffer.is_empty(), &current_line, cursor)?;
                    }
                    // Escape clears the line being typed; Ctrl+C is the way out.
                    KeyCode::Esc => {
                        current_line.clear();
                        cursor = 0;
                        history_position = None;
                        redraw_input(&mut stdout, &mut rows, buffer.is_empty(), &current_line, cursor)?;
                    }
                    _ => {}
                }
                stdout.flush()?;
            }
            KeyInput::Paste(text) => {
                // The input is one screen line per buffer line; pasted line breaks become spaces.
                insert_at_cursor(&mut current_line, &mut cursor, &text.replace("\r\n", " ").replace(['\n', '\r'], " "));
                history_position = None;
                redraw_input(&mut stdout, &mut rows, buffer.is_empty(), &current_line, cursor)?;
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_input_lines_wrap_onto_rows() {
        // "> " plus 8 characters in a 10-wide terminal fills exactly one row.
        assert_eq!(input_layout(10, 10, 10), (1, 1, 0));
        assert_eq!(input_layout(10, 10, 4), (1, 0, 4));
        assert_eq!(input_layout(10, 25, 12), (2, 1, 2));
        assert_eq!(input_layout(80, 5, 5), (0, 0, 5));
    }

    #[test]
    fn supports_backslash_continuation_before_submission() {
        let mut buffer = String::new();
        assert!(accept_line(&mut buffer, "12 \\"));
        assert_eq!(buffer, "12 \n");
        assert!(!accept_line(&mut buffer, "dup"));
        assert_eq!(buffer, "12 \ndup");
    }

    #[test]
    fn continuation_lines_have_no_prompt_marker() {
        assert_eq!(prompt(false), "");
        assert_eq!(prompt(true), "> ");
    }

    #[test]
    fn continuation_indent_matches_the_prompt_width() {
        assert_eq!(CONTINUATION_INDENT.len(), "> ".len());
    }

    #[test]
    fn beginning_a_continuation_indents_the_new_source_line() {
        let mut buffer = String::new();
        let mut current_line = String::from("1 +");
        begin_continuation(&mut buffer, &mut current_line);
        assert_eq!(buffer, "1 +\n");
        assert_eq!(current_line, "  ");
    }

    #[test]
    fn shift_enter_and_legacy_alt_enter_are_multiline_keys() {
        assert!(is_multiline_enter(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::SHIFT
        )));
        assert!(is_multiline_enter(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::ALT
        )));
        assert!(is_multiline_enter(KeyEvent::new(
            KeyCode::Char('j'),
            KeyModifiers::CONTROL
        )));
        assert!(!is_multiline_enter(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE
        )));
    }
}

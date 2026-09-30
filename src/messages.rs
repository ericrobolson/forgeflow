use std::{
    error::Error,
    io::{self, IsTerminal, Write},
};

use crossterm::{
    event::{
        self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
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
    workflow::{self, Context},
};

const CONTINUATION_INDENT: &str = "  ";

/// A turn-by-turn interface: ordinary messages go to chat; `;` prefixed messages run as code.
pub struct Conversation {
    context: Context,
    backend: OpenAiCompatible,
    history: Vec<Exchange>,
    styled: bool,
}

/// Start a conversation with the selected model.
///
/// Uses Crossterm key events for a real terminal and line input when stdin is
/// piped or redirected. Shift+Enter inserts a newline where the terminal
/// reports that key combination; a trailing backslash is a portable fallback.
pub fn run(mut context: Context) -> Result<(), Box<dyn Error>> {
    let terminal = io::stdin().is_terminal();
    // Print the banner before the server starts, so early keystrokes land after it.
    if terminal {
        println!(
            "ForgeFlow. Enter sends; prefix a program with ; to compile and run it. Shift+Enter adds a line where supported; Ctrl+C exits."
        );
    } else {
        println!("ForgeFlow. Send a message; prefix a program with ; to compile and run it; Ctrl+D to exit.");
    }
    // Start the model server now so the first message does not wait for it.
    let (backend, _) = chat::connect(&mut context)?;
    let mut conversation = Conversation {
        context,
        backend,
        history: vec![],
        styled: io::stdout().is_terminal(),
    };
    if terminal {
        run_terminal(&mut conversation)
    } else {
        run_stream(&mut conversation)
    }
}

fn report_submission(
    buffer: &mut String,
    _submission: usize,
    terminal_mode: bool,
    conversation: &mut Conversation,
) -> Result<(), Box<dyn Error>> {
    let message = std::mem::take(buffer);
    // Crossterm raw mode disables the terminal's normal LF-to-CRLF output
    // handling. Run the turn in cooked mode, then resume raw key input.
    if terminal_mode {
        disable_raw_mode()?;
    }
    if !message.trim().is_empty() {
        conversation.turn(message.trim());
    }
    if terminal_mode {
        enable_raw_mode()?;
    }
    Ok(())
}

impl Conversation {
    fn dim(&self, text: &str) -> String {
        if self.styled {
            format!("\x1b[2m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    /// Chat commands such as `/model`, handled directly and never sent to the model.
    fn command(&mut self, message: &str) {
        let words: Vec<&str> = message.split_whitespace().collect();
        let result = match words.as_slice() {
            ["/model"] => {
                let current = Config::load(&self.context.project.folder())
                    .ok()
                    .and_then(|c| c.model)
                    .unwrap_or_else(|| "(none)".into());
                println!("Current model: {current}\nInstalled:");
                for line in models::installed_summary() {
                    println!("  {line}");
                }
                println!("Switch with /model <id>; see every model with /models.");
                Ok(())
            }
            ["/model", id] => self.switch_model(id),
            ["/models"] => workflow::run_steps(&mut self.context, &[Ops::Models]),
            ["/pull", id] => models::pull(id, &mut io::stderr()).map(|_| ()),
            _ => Err(format!(
                "unknown command `{message}`; try /model, /model <id>, /models, /pull <id>, or /help"
            )),
        };
        if let Err(error) = result {
            eprintln!("error: {error}");
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
                println!("{}", self.dim(&format!("switched to {id} in {} ms", started.elapsed().as_millis())));
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
    fn turn(&mut self, message: &str) {
        if message.starts_with('/') && message != "/help" {
            return self.command(message);
        }
        if let Some(request) = message.trim_start().strip_prefix(';') {
            return self.run_program_request(request);
        }
        self.chat(message);
    }

    /// Sends ordinary messages directly to the model with no tools available.
    fn chat(&mut self, message: &str) {
        let mut messages = vec![json!({
            "role": "system",
            "content": "You are ForgeFlow, a helpful conversational assistant. Answer the user's message directly. No tools are available in chat. To run ForgeFlow code, the user starts a message with a semicolon."
        })];
        for exchange in &self.history {
            messages.push(json!({"role": "user", "content": exchange.message}));
            messages.push(json!({"role": "assistant", "content": exchange.output}));
        }
        messages.push(json!({"role": "user", "content": message}));

        let mut output = String::new();
        let turn = self.backend.step(
            &messages,
            &[],
            &StepOptions {
                slot: Some(crate::server::ANSWER_SLOT),
                ..StepOptions::default()
            },
            &mut |delta| {
                if let Delta::Text(text) = delta {
                    print!("{text}");
                    io::stdout().flush().ok();
                    output.push_str(&text);
                }
            },
        );
        match turn {
            Ok(turn) => {
                if !output.is_empty() {
                    println!();
                }
                if !turn.tool_calls.is_empty() {
                    eprintln!("error: chat does not run tools; prefix ForgeFlow code with `;` to execute it");
                }
                self.history.push(Exchange {
                    message: message.to_string(),
                    program: String::new(),
                    output: turn.content,
                });
            }
            Err(error) => eprintln!("error: {error}"),
        }
    }

    /// Translates a semicolon-prefixed request to a checked ForgeFlow program and runs it.
    fn run_program_request(&mut self, request: &str) {
        let started = Instant::now();
        let (program, steps, attempts) = match compile_direct_program(request) {
            Some(steps) => (request.trim().to_string(), steps, 0),
            None => {
                let translated = match translate::translate(
                    &self.backend,
                    &self.history,
                    request.trim(),
                    &self.context.registers,
                ) {
                    Ok(translated) => translated,
                    Err(error) => return eprintln!("error: {error}"),
                };
                (translated.program, translated.steps, translated.attempts)
            }
        };
        let translated_in = started.elapsed();
        println!("{}", self.dim(&format!("→ {program}")));

        let running = Instant::now();
        self.context.stack.clear();
        self.context.take_printed();
        self.context.store_label = program.clone();
        if let Err(error) = workflow::run_steps(&mut self.context, &steps) {
            eprintln!("error: {error}");
            self.context.printed.push_str(&format!("error: {error}\n"));
        }
        for value in std::mem::take(&mut self.context.stack) {
            self.context.say(&value.to_text());
        }
        println!("{}", self.dim(&timing(translated_in, running.elapsed(), attempts)));
        self.history.push(Exchange {
            message: format!(";{}", request.trim()),
            program,
            output: self.context.take_printed(),
        });
    }
}

/// Uses valid source after `;` as code directly; natural language falls through to translation.
fn compile_direct_program(source_text: &str) -> Option<Vec<Ops>> {
    let source = Source::new("<code>", source_text);
    let parsed = parser::parse(&source).ok()?;
    let steps = words::compile(&parsed.tokens).ok()?;
    workflow::check_steps(vec![], &steps).ok()?;
    Some(steps)
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

fn run_stream(conversation: &mut Conversation) -> Result<(), Box<dyn Error>> {
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

fn run_terminal(conversation: &mut Conversation) -> Result<(), Box<dyn Error>> {
    let _raw_mode = RawMode::enter()?;
    // Request unambiguous reports for modified keys, including Shift+Enter.
    // Other terminals ignore the request and retain the trailing-backslash
    // continuation fallback.
    let _keyboard_enhancement = KeyboardEnhancement::try_enable();
    let mut stdout = io::stdout();
    let mut buffer = String::new();
    let mut current_line = String::new();
    let mut submission = 1;
    print!("> ");
    stdout.flush()?;

    loop {
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    print!("\r\n");
                    stdout.flush()?;
                    break;
                }
                if is_multiline_enter(key) {
                    begin_continuation(&mut buffer, &mut current_line);
                    print!("\r\n{current_line}");
                    stdout.flush()?;
                    continue;
                }
                match key.code {
                    KeyCode::Enter => {
                        if current_line.ends_with('\\') {
                            current_line.pop();
                            begin_continuation(&mut buffer, &mut current_line);
                            print!("\r\n{current_line}");
                        } else {
                            buffer.push_str(&current_line);
                            print!("\r\n");
                            report_submission(&mut buffer, submission, true, conversation)?;
                            submission += 1;
                            current_line.clear();
                            print!("> ");
                        }
                    }
                    KeyCode::Backspace => {
                        current_line.pop();
                        print!("\r\x1b[2K{}{}", prompt(buffer.is_empty()), current_line);
                    }
                    KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        current_line.push(ch);
                        print!("{ch}");
                    }
                    KeyCode::Esc => {
                        print!("\r\n");
                        stdout.flush()?;
                        break;
                    }
                    _ => {}
                }
                stdout.flush()?;
            }
            Event::Paste(text) => {
                current_line.push_str(&text);
                print!("{text}");
                stdout.flush()?;
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

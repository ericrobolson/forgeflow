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

use crate::parser::{self, Source};

const CONTINUATION_INDENT: &str = "  ";

/// Start ForgeFlow's interactive parser REPL.
///
/// Uses Crossterm key events for a real terminal and line input when stdin is
/// piped or redirected. Shift+Enter inserts a newline where the terminal
/// reports that key combination; a trailing backslash is a portable fallback.
pub fn run() -> Result<(), Box<dyn Error>> {
    if io::stdin().is_terminal() {
        run_terminal()
    } else {
        run_stream()
    }
}

fn report_submission(
    buffer: &mut String,
    submission: usize,
    terminal_mode: bool,
) -> Result<(), Box<dyn Error>> {
    let source = Source::new(format!("<repl:{submission}>"), std::mem::take(buffer));
    if let Err(errors) = parser::parse(&source) {
        // Crossterm raw mode disables the terminal's normal LF-to-CRLF output
        // handling. Let Ariadne print diagnostics in cooked mode, then resume
        // raw key input for the next prompt.
        if terminal_mode {
            disable_raw_mode()?;
        }
        parser::print_errors(&source, &errors);
        if terminal_mode {
            enable_raw_mode()?;
        }
    }
    Ok(())
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

fn run_stream() -> Result<(), Box<dyn Error>> {
    println!("ForgeFlow parser REPL. Enter a line to parse; Ctrl+D to exit.");
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
        report_submission(&mut buffer, submission, false)?;
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

fn run_terminal() -> Result<(), Box<dyn Error>> {
    println!(
        "ForgeFlow parser REPL. Enter submits; Shift+Enter adds a line where supported, Ctrl+J also adds a line; Ctrl+C exits."
    );
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
                            report_submission(&mut buffer, submission, true)?;
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

    #[test]
    fn multiline_buffer_parses_as_one_submission() {
        let source = Source::new("<repl:1>", "12\ndup");
        assert_eq!(parser::parse(&source).unwrap().tokens.len(), 2);
    }

    #[test]
    fn shift_enter_style_newline_joins_tokens_across_lines() {
        let source = Source::new("<repl:1>", "12\ndup");
        assert_eq!(parser::parse(&source).unwrap().tokens.len(), 2);
    }
}

use std::{
    error::Error,
    fs,
    io::{self, IsTerminal, Write},
    path::Path,
};

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode},
};
use forgeflow::parser::{self, ParseOutput, Source};

fn parse_and_report(source: &Source, show_tokens: bool) -> Result<ParseOutput, ()> {
    match parser::parse(source) {
        Ok(output) => {
            if show_tokens {
                for token in &output.tokens {
                    println!("{}", parser::format_token(token));
                }
            }
            Ok(output)
        }
        Err(errors) => {
            parser::print_errors(source, &errors);
            Err(())
        }
    }
}

fn run_file(path: &Path) -> Result<(), Box<dyn Error>> {
    let source = read_source(path)?;
    parse_and_report(&source, true)
        .map(|_| ())
        .map_err(|_| "source parse failed".into())
}

fn read_source(path: &Path) -> io::Result<Source> {
    Ok(Source::new(
        path.display().to_string(),
        fs::read_to_string(path)?,
    ))
}

fn run_repl() -> Result<(), Box<dyn Error>> {
    if io::stdin().is_terminal() {
        run_terminal_repl()
    } else {
        run_stream_repl()
    }
}

fn accept_submission(buffer: &mut String, line: &str) -> bool {
    buffer.push_str(line);
    if buffer.ends_with('\\') {
        buffer.pop();
        buffer.push('\n');
        true
    } else {
        false
    }
}

fn submit_repl_buffer(buffer: &mut String, submission: usize) {
    let source = Source::new(format!("<repl:{submission}>"), std::mem::take(buffer));
    let _ = parse_and_report(&source, false);
}

fn run_stream_repl() -> Result<(), Box<dyn Error>> {
    println!("ForgeFlow parser REPL. Enter a line to parse; Ctrl+D to exit.");
    let stdin = io::stdin();
    let mut buffer = String::new();
    let mut line = String::new();
    let mut submission = 1;
    loop {
        print!("{}", if buffer.is_empty() { "> " } else { "... " });
        io::stdout().flush()?;
        line.clear();
        if stdin.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end_matches(|ch| ch == '\r' || ch == '\n');
        if accept_submission(&mut buffer, line) {
            continue;
        }
        submit_repl_buffer(&mut buffer, submission);
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

fn run_terminal_repl() -> Result<(), Box<dyn Error>> {
    println!("ForgeFlow parser REPL. Enter parses; Shift+Enter inserts a newline; Ctrl+C exits.");
    let _raw_mode = RawMode::enter()?;
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
                    println!();
                    break;
                }
                match key.code {
                    KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                        buffer.push_str(&current_line);
                        buffer.push('\n');
                        current_line.clear();
                        print!("\r\n... ");
                    }
                    KeyCode::Enter => {
                        if current_line.ends_with('\\') {
                            current_line.pop();
                            buffer.push_str(&current_line);
                            buffer.push('\n');
                            current_line.clear();
                            print!("\r\n... ");
                        } else {
                            buffer.push_str(&current_line);
                            println!();
                            submit_repl_buffer(&mut buffer, submission);
                            submission += 1;
                            current_line.clear();
                            print!("> ");
                        }
                    }
                    KeyCode::Backspace => {
                        current_line.pop();
                        print!(
                            "\r\x1b[2K{}{}",
                            if buffer.is_empty() { "> " } else { "... " },
                            current_line
                        );
                    }
                    KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        current_line.push(ch);
                        print!("{ch}");
                    }
                    KeyCode::Esc => {
                        println!();
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

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    match (args.next(), args.next()) {
        (Some(path), None) => run_file(Path::new(&path)),
        (None, None) => run_repl(),
        _ => {
            eprintln!("Usage: forgeflow [SOURCE_FILE]");
            Err("expected at most one source file".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_mode_formats_parsed_tokens_with_spans() {
        let source = Source::new("example.ff", "12 dup");
        let output = parse_and_report(&source, false).unwrap();
        assert_eq!(parser::format_token(&output.tokens[0]), "0..2 Integer(12)");
        assert_eq!(
            parser::format_token(&output.tokens[1]),
            "3..6 Identifier(dup)"
        );
    }

    #[test]
    fn file_mode_reads_source_with_its_path_as_the_name() {
        let path = std::env::temp_dir().join(format!(
            "forgeflow-parser-{}-file-test.ff",
            std::process::id()
        ));
        fs::write(&path, "12 dup").unwrap();
        let source = read_source(&path).unwrap();
        assert_eq!(source.name, path.display().to_string());
        assert_eq!(parser::parse(&source).unwrap().tokens.len(), 2);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn repl_submission_supports_backslash_continuation_and_submission() {
        let mut buffer = String::new();
        assert!(accept_submission(&mut buffer, "12 \\"));
        assert_eq!(buffer, "12 \n");
        assert!(!accept_submission(&mut buffer, "dup"));
        assert_eq!(buffer, "12 \ndup");
    }

    #[test]
    fn repl_buffer_can_parse_multiline_submission() {
        let source = Source::new("<repl:1>", "12\ndup");
        assert_eq!(parser::parse(&source).unwrap().tokens.len(), 2);
    }

    #[test]
    fn repl_shift_enter_style_newline_joins_tokens_across_lines() {
        let mut source_text = String::from("12\n");
        source_text.push_str("dup");
        let source = Source::new("<repl:1>", source_text);
        assert_eq!(parser::parse(&source).unwrap().tokens.len(), 2);
    }
}

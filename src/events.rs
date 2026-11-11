//! What a conversation shows the user, as events, so the terminal and the web UI render
//! the same turns.

use std::io::{self, Write};

/// What a piece of output is, which decides its colour or style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A model's streamed reply.
    Chat,
    /// Output of a running program.
    Run,
    /// Suggested commands.
    Suggest,
    /// The stack after a program.
    Stack,
    /// Output of a `/` command.
    Command,
    /// Secondary detail: the translated program, timings, context left.
    Note,
    /// Progress of slow work, such as image generation.
    Progress,
    Error,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Chat => "chat",
            Kind::Run => "run",
            Kind::Suggest => "suggest",
            Kind::Stack => "stack",
            Kind::Command => "command",
            Kind::Note => "note",
            Kind::Progress => "progress",
            Kind::Error => "error",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Part of a line, such as a streamed token; the text carries its own newlines.
    Text(Kind, String),
    /// A whole line.
    Line(Kind, String),
    /// An image saved in the project, by project-relative path.
    Image(String),
    /// A video saved in the project, by project-relative path.
    Video(String),
}

/// Where a conversation's events go.
pub trait Sink: Send {
    fn emit(&mut self, event: Event);
}

/// Prints events, coloured when stdout is a terminal. Errors and progress go to stderr.
pub struct Terminal {
    pub styled: bool,
}

impl Terminal {
    pub fn new() -> Self {
        use std::io::IsTerminal;
        Self { styled: io::stdout().is_terminal() }
    }

    fn color(kind: Kind) -> &'static str {
        match kind {
            Kind::Chat => "\x1b[36m",
            Kind::Run => "\x1b[32m",
            Kind::Suggest => "\x1b[33m",
            Kind::Stack => "\x1b[34m",
            Kind::Command => "\x1b[35m",
            Kind::Note => "\x1b[2m",
            Kind::Progress | Kind::Error => "",
        }
    }

    fn styled_text(&self, kind: Kind, text: &str) -> String {
        let color = Self::color(kind);
        if self.styled && !color.is_empty() {
            format!("{color}{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }
}

impl Default for Terminal {
    fn default() -> Self {
        Self::new()
    }
}

impl Sink for Terminal {
    fn emit(&mut self, event: Event) {
        match event {
            Event::Text(kind, text) => {
                print!("{}", self.styled_text(kind, &text));
                io::stdout().flush().ok();
            }
            Event::Line(Kind::Error, text) => eprintln!("error: {text}"),
            Event::Line(Kind::Progress, text) => eprintln!("{text}"),
            Event::Line(kind, text) => println!("{}", self.styled_text(kind, &text)),
            // The saved path reaches the terminal through the stack or `print`.
            Event::Image(_) | Event::Video(_) => {}
        }
    }
}

/// Collects events, for tests.
#[derive(Default)]
pub struct Recorder(pub std::sync::Arc<std::sync::Mutex<Vec<Event>>>);

impl Sink for Recorder {
    fn emit(&mut self, event: Event) {
        self.0.lock().expect("recorder lock").push(event);
    }
}

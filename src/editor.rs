//! `forgeflow edit <path>` and `/edit <path>`: the file on the left and its pseudocode
//! summary on the right, scrolled together line for line. Summaries come from `summary`,
//! generated on a background thread so the editor stays responsive.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use crossterm::{
    event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    terminal::{Clear as ClearScreen, ClearType},
};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Wrap},
};
use walkdir::WalkDir;

use crate::{
    chat::OpenAiCompatible,
    project::Project,
    server::LocalServer,
    summary::{Item, Progress, Summarizer, Summary},
    workflow::Context,
};

const READ_MAX_BYTES: u64 = 2_000_000;
const OVERVIEW_MAX_LINES: usize = 4;
const PICKER_MAX_FILES: usize = 20_000;
const PICKER_SHOWN: usize = 200;
/// Folders the file picker never lists, besides hidden ones and generated `_forgeflow` folders.
const PICKER_SKIPPED: [&str; 2] = ["target", "node_modules"];
const TAB: &str = "    ";
const HELP: &str = "` summary · Ctrl+S save · Ctrl+R re-summarize · Ctrl+O open · Esc quit";
const SUMMARY_HELP: &str = "j/k or ↑/↓ jump between blocks · ←/→ next or previous line · `, Enter or Esc edit here · Ctrl+S save";
/// Lines kept above a block the summary pane jumps to.
const JUMP_MARGIN: usize = 2;

/// Opens `relative` in the editor until the user quits. The file may not exist yet; saving creates it.
pub fn run(context: &mut Context, relative: &str) -> Result<(), String> {
    let (summarizer, status) = match Summarizer::new(context) {
        Ok(summarizer) => (Some(summarizer), Status::Idle),
        Err(error) => (None, Status::Unavailable(error)),
    };
    let mut editor = Editor {
        project: Project { root: context.project.root.clone() },
        relative: String::new(),
        path: PathBuf::new(),
        buffer: Buffer::new(""),
        scroll: 0,
        hscroll: 0,
        view_height: 1,
        edited: false,
        summary: None,
        summary_current: false,
        status,
        summarizer,
        job: None,
        queued: None,
        launching: false,
        server: None,
        message: None,
        confirm: None,
        picker: None,
        focus: Focus::Source,
    };
    // Open before taking over the screen, so a bad path is reported like any other error.
    editor.open(relative.trim())?;
    let mut terminal = ratatui::try_init().map_err(|e| e.to_string())?;
    // Clearing directly, not with `terminal.clear()`, which asks the terminal for the cursor
    // position. Terminals without an alternate screen would otherwise show earlier output.
    let _ = crossterm::execute!(std::io::stdout(), ClearScreen(ClearType::All), EnableBracketedPaste);
    let result = editor.event_loop(&mut terminal);
    let _ = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    // A server still loading would be left running if this process exited first.
    if editor.launching && let Some(receiver) = editor.job.take() {
        eprintln!("Waiting for llama-server to finish starting…");
        while let Ok(update) = receiver.recv() {
            match update {
                Update::Server(server, _) => {
                    editor.server = Some(server);
                    break;
                }
                Update::Done(_) => break,
                Update::Progress(_) => {}
            }
        }
    }
    // Keep a server the editor started, so chat and later summaries reuse it.
    if let Some(server) = editor.server.take() {
        context.server = Some(server);
    }
    result
}

/// Lines of text with a cursor. Columns count characters, not bytes.
struct Buffer {
    lines: Vec<String>,
    row: usize,
    col: usize,
    /// The column Up and Down aim for across shorter lines.
    goal: usize,
    crlf: bool,
    trailing_newline: bool,
}

impl Buffer {
    fn new(text: &str) -> Self {
        let body = text.strip_suffix('\n').unwrap_or(text);
        let lines = body.split('\n').map(|line| line.strip_suffix('\r').unwrap_or(line).to_string()).collect();
        Self {
            lines,
            row: 0,
            col: 0,
            goal: 0,
            crlf: text.contains("\r\n"),
            // A new file gets a final newline, like most editors write.
            trailing_newline: text.is_empty() || text.ends_with('\n'),
        }
    }

    fn text(&self) -> String {
        let newline = if self.crlf { "\r\n" } else { "\n" };
        let mut text = self.lines.join(newline);
        if self.trailing_newline {
            text.push_str(newline);
        }
        text
    }

    fn line_len(&self, row: usize) -> usize {
        self.lines[row].chars().count()
    }

    fn byte(&self, row: usize, col: usize) -> usize {
        let line = &self.lines[row];
        line.char_indices().nth(col).map_or(line.len(), |(i, _)| i)
    }

    /// Inserts text as typed or pasted; newlines split lines without indenting.
    fn insert(&mut self, text: &str) {
        for ch in text.chars() {
            match ch {
                '\n' => self.split_line(String::new()),
                '\r' => {}
                _ => {
                    let at = self.byte(self.row, self.col);
                    self.lines[self.row].insert(at, ch);
                    self.col += 1;
                }
            }
        }
        self.goal = self.col;
    }

    /// Enter: splits the line, carrying the current line's indentation to the new one.
    fn newline(&mut self) {
        let indent: String = self.lines[self.row].chars().take(self.col).take_while(|c| c.is_whitespace()).collect();
        self.split_line(indent);
        self.goal = self.col;
    }

    fn split_line(&mut self, indent: String) {
        let at = self.byte(self.row, self.col);
        let rest = self.lines[self.row].split_off(at);
        self.col = indent.chars().count();
        self.row += 1;
        self.lines.insert(self.row, indent + &rest);
    }

    fn backspace(&mut self) {
        if self.col > 0 {
            let (start, end) = (self.byte(self.row, self.col - 1), self.byte(self.row, self.col));
            self.lines[self.row].replace_range(start..end, "");
            self.col -= 1;
        } else if self.row > 0 {
            let line = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.line_len(self.row);
            self.lines[self.row].push_str(&line);
        }
        self.goal = self.col;
    }

    fn delete(&mut self) {
        if self.col < self.line_len(self.row) {
            let (start, end) = (self.byte(self.row, self.col), self.byte(self.row, self.col + 1));
            self.lines[self.row].replace_range(start..end, "");
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&next);
        }
    }

    fn left(&mut self, by_word: bool) {
        if self.col == 0 {
            if self.row > 0 {
                self.row -= 1;
                self.col = self.line_len(self.row);
            }
        } else if by_word {
            let chars: Vec<char> = self.lines[self.row].chars().collect();
            let mut col = self.col;
            while col > 0 && !is_word_char(chars[col - 1]) {
                col -= 1;
            }
            while col > 0 && is_word_char(chars[col - 1]) {
                col -= 1;
            }
            self.col = col;
        } else {
            self.col -= 1;
        }
        self.goal = self.col;
    }

    fn right(&mut self, by_word: bool) {
        let len = self.line_len(self.row);
        if self.col == len {
            if self.row + 1 < self.lines.len() {
                self.row += 1;
                self.col = 0;
            }
        } else if by_word {
            let chars: Vec<char> = self.lines[self.row].chars().collect();
            let mut col = self.col;
            while col < len && !is_word_char(chars[col]) {
                col += 1;
            }
            while col < len && is_word_char(chars[col]) {
                col += 1;
            }
            self.col = col;
        } else {
            self.col += 1;
        }
        self.goal = self.col;
    }

    /// Moves `delta` lines up (negative) or down, aiming for the goal column.
    fn vertical(&mut self, delta: isize) {
        let last = self.lines.len() - 1;
        self.row = self.row.saturating_add_signed(delta).min(last);
        self.col = self.goal.min(self.line_len(self.row));
    }

    fn home(&mut self) {
        // First press goes to the first non-blank character, the next to column 0.
        let indent = self.lines[self.row].chars().take_while(|c| c.is_whitespace()).count();
        self.col = if self.col == indent { 0 } else { indent };
        self.goal = self.col;
    }

    fn end(&mut self) {
        self.col = self.line_len(self.row);
        self.goal = self.col;
    }
}

/// A backtick toggles summary mode. Shift+` is `~`, which still types, including where the
/// kitty keyboard protocol (which chat turns on) reports it as Shift+`.
fn is_toggle(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('`') && !key.modifiers.contains(KeyModifiers::SHIFT)
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// What the summary pane is doing, shown in its title.
enum Status {
    Idle,
    /// No model to summarize with; the editor still works.
    Unavailable(String),
    Cached,
    Working { started: Instant, progress: String },
    Done(Duration),
    Failed(String),
}

enum Update {
    Progress(Progress),
    /// The job started llama-server; the editor keeps it alive.
    Server(LocalServer, OpenAiCompatible),
    Done(Result<Summary, String>),
}

/// Which pane keys go to. In summary mode, j and k move between blocks instead of typing.
#[derive(Clone, Copy, PartialEq)]
enum Focus {
    Source,
    Summary,
}

#[derive(Clone, Copy, PartialEq)]
enum Confirm {
    Quit,
    Open,
}

struct Picker {
    query: String,
    files: Vec<String>,
    selected: usize,
}

impl Picker {
    fn new(project: &Project) -> Self {
        let root = project.root.as_path();
        let generated = [project.sessions_folder(), project.summaries_folder()];
        let files = WalkDir::new(root)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|entry| {
                let name = entry.file_name().to_string_lossy();
                let skipped_folder = entry.file_type().is_dir() && (PICKER_SKIPPED.contains(&name.as_ref()) || generated.iter().any(|g| g == entry.path()));
                entry.depth() == 0 || !(name.starts_with('.') || skipped_folder)
            })
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .filter_map(|entry| entry.path().strip_prefix(root).ok().map(|p| p.to_string_lossy().into_owned()))
            .take(PICKER_MAX_FILES)
            .collect();
        Self { query: String::new(), files, selected: 0 }
    }

    /// Files containing the query's characters in order, those with it in the file name first.
    fn matches(&self) -> Vec<&str> {
        let query = self.query.to_lowercase();
        let mut found: Vec<&str> = self.files.iter().map(String::as_str).filter(|path| is_subsequence(&query, &path.to_lowercase())).collect();
        found.sort_by_key(|path| {
            let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
            (!name.contains(&query), path.len())
        });
        found.truncate(PICKER_SHOWN);
        found
    }
}

fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut chars = haystack.chars();
    needle.chars().all(|n| chars.any(|h| h == n))
}

struct Editor {
    project: Project,
    relative: String,
    path: PathBuf,
    buffer: Buffer,
    scroll: usize,
    hscroll: usize,
    /// Visible source lines, from the last draw; used by Page Up and Page Down.
    view_height: usize,
    edited: bool,
    summary: Option<Summary>,
    /// Whether the summary describes the current text; edits make it out of date until saved.
    summary_current: bool,
    status: Status,
    summarizer: Option<Summarizer>,
    job: Option<Receiver<Update>>,
    /// A summary requested while a job ran, started when it finishes; true forces regeneration.
    queued: Option<bool>,
    /// Whether the running job is starting llama-server.
    launching: bool,
    server: Option<LocalServer>,
    message: Option<String>,
    confirm: Option<Confirm>,
    picker: Option<Picker>,
    focus: Focus,
}

impl Editor {
    fn open(&mut self, relative: &str) -> Result<(), String> {
        let path = self.project.resolve(relative)?;
        let text = if path.exists() {
            if path.is_dir() {
                return Err(format!("{relative} is a folder"));
            }
            let size = std::fs::metadata(&path).map_err(|e| format!("{relative}: {e}"))?.len();
            if size > READ_MAX_BYTES {
                return Err(format!("{relative} is {size} bytes; the limit is {READ_MAX_BYTES}"));
            }
            std::fs::read_to_string(&path).map_err(|e| format!("{relative}: {e}"))?
        } else {
            String::new()
        };
        self.relative = relative.to_string();
        self.path = path;
        self.buffer = Buffer::new(&text);
        self.scroll = 0;
        self.hscroll = 0;
        self.edited = false;
        self.summary = None;
        self.summary_current = false;
        self.message = None;
        if !matches!(self.status, Status::Unavailable(_)) {
            self.status = Status::Idle;
        }
        self.start_summary(false);
        Ok(())
    }

    /// Shows the cached summary of the current text, or starts generating one.
    fn start_summary(&mut self, force: bool) {
        let Some(mut summarizer) = self.summarizer.clone() else { return };
        // One request at a time: the model serves them one by one anyway, and two jobs
        // must not both start a server.
        if self.job.is_some() {
            self.queued = Some(force || self.queued.unwrap_or(false));
            return;
        }
        let text = self.buffer.text();
        self.summary_current = true;
        if !force && let Some(summary) = summarizer.cached(&text) {
            self.summary = Some(summary);
            self.status = Status::Cached;
            return;
        }
        let (sender, receiver) = mpsc::channel();
        let label = self.relative.clone();
        self.launching = summarizer.backend.is_none();
        std::thread::spawn(move || {
            let (backend, server) = match summarizer.backend_or_start() {
                Ok(started) => started,
                Err(error) => {
                    let _ = sender.send(Update::Done(Err(error)));
                    return;
                }
            };
            if let Some(server) = server {
                // If the editor has closed, the server drops here and stops.
                if sender.send(Update::Server(server, backend.clone())).is_err() {
                    return;
                }
            }
            summarizer.backend = Some(backend);
            let progress = sender.clone();
            let result = summarizer.summarize(&label, &text, force, &mut |update| {
                let _ = progress.send(Update::Progress(update));
            });
            let _ = sender.send(Update::Done(result));
        });
        self.job = Some(receiver);
        let progress = if self.launching { "starting the model" } else { "starting" };
        self.status = Status::Working { started: Instant::now(), progress: progress.into() };
    }

    fn poll_job(&mut self) {
        let Some(receiver) = &self.job else { return };
        let mut finished = false;
        while let Ok(update) = receiver.try_recv() {
            match update {
                Update::Progress(progress) => {
                    if let Status::Working { progress: text, .. } = &mut self.status {
                        *text = match progress {
                            Progress::Window { total: 1, .. } => "reading".into(),
                            Progress::Window { index, total } => format!("part {} of {total}", index + 1),
                            Progress::Overview => "overview".into(),
                            // Keep the part, replacing any previous token count.
                            Progress::Tokens(tokens) => format!("{} · {tokens} tokens", text.split(" · ").next().unwrap_or("")),
                        };
                    }
                }
                Update::Server(server, backend) => {
                    self.server = Some(server);
                    self.launching = false;
                    if let Some(summarizer) = &mut self.summarizer {
                        summarizer.backend = Some(backend);
                    }
                }
                Update::Done(result) => {
                    let elapsed = match &self.status {
                        Status::Working { started, .. } => started.elapsed(),
                        _ => Duration::ZERO,
                    };
                    match result {
                        Ok(summary) => {
                            self.summary = Some(summary);
                            self.status = Status::Done(elapsed);
                        }
                        Err(error) => self.status = Status::Failed(error),
                    }
                    finished = true;
                }
            }
        }
        if finished {
            self.job = None;
            self.launching = false;
            if let Some(force) = self.queued.take() {
                self.start_summary(force);
            }
        }
    }

    fn event_loop(&mut self, terminal: &mut DefaultTerminal) -> Result<(), String> {
        loop {
            self.poll_job();
            terminal.draw(|frame| self.draw(frame)).map_err(|e| e.to_string())?;
            // Wake regularly so summary progress shows without a key press.
            if !event::poll(Duration::from_millis(100)).map_err(|e| e.to_string())? {
                continue;
            }
            match event::read().map_err(|e| e.to_string())? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if self.handle_key(key) {
                        return Ok(());
                    }
                }
                Event::Paste(text) if self.picker.is_none() => {
                    self.buffer.insert(&text);
                    self.mark_edited();
                }
                _ => {}
            }
        }
    }

    fn mark_edited(&mut self) {
        self.edited = true;
        self.summary_current = false;
    }

    /// Handles one key; returns true when the editor should close.
    fn handle_key(&mut self, key: KeyEvent) -> bool {
        if self.picker.is_some() {
            self.handle_picker_key(key);
            return false;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // Option+Arrow on macOS arrives as Alt or Meta depending on the terminal.
        let by_word = key.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::META | KeyModifiers::SUPER);
        let confirm = self.confirm.take();
        self.message = None;
        if self.focus == Focus::Summary && !ctrl {
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => self.jump(true),
                KeyCode::Char('k') | KeyCode::Up => self.jump(false),
                KeyCode::Left => self.step_line(false),
                KeyCode::Right => self.step_line(true),
                KeyCode::Enter | KeyCode::Esc => self.focus = Focus::Source,
                _ if is_toggle(key) => self.focus = Focus::Source,
                _ => {}
            }
            return false;
        }
        match key.code {
            _ if is_toggle(key) && !ctrl => {
                if self.summary.is_some() {
                    self.focus = Focus::Summary;
                    self.snap_to_summary_line();
                } else {
                    self.message = Some("No summary yet.".into());
                }
            }
            KeyCode::Esc | KeyCode::Char('q' | 'c') if key.code == KeyCode::Esc || ctrl => {
                if self.edited && confirm != Some(Confirm::Quit) {
                    self.message = Some("Unsaved changes: Ctrl+S saves them, Esc again discards them.".into());
                    self.confirm = Some(Confirm::Quit);
                } else {
                    return true;
                }
            }
            KeyCode::Char('s') if ctrl => self.save(),
            KeyCode::Char('r') if ctrl => {
                if self.summarizer.is_some() {
                    self.start_summary(true);
                } else if let Status::Unavailable(error) = &self.status {
                    self.message = Some(format!("No summaries: {error}"));
                }
            }
            KeyCode::Char('o') if ctrl => {
                if self.edited && confirm != Some(Confirm::Open) {
                    self.message = Some("Unsaved changes: Ctrl+S saves them, Ctrl+O again opens another file anyway.".into());
                    self.confirm = Some(Confirm::Open);
                } else {
                    self.picker = Some(Picker::new(&self.project));
                }
            }
            KeyCode::Enter => {
                self.buffer.newline();
                self.mark_edited();
            }
            KeyCode::Tab => {
                self.buffer.insert(TAB);
                self.mark_edited();
            }
            KeyCode::Backspace => {
                self.buffer.backspace();
                self.mark_edited();
            }
            KeyCode::Delete => {
                self.buffer.delete();
                self.mark_edited();
            }
            KeyCode::Left => self.buffer.left(by_word),
            KeyCode::Right => self.buffer.right(by_word),
            KeyCode::Up => self.buffer.vertical(-1),
            KeyCode::Down => self.buffer.vertical(1),
            KeyCode::PageUp => self.buffer.vertical(-(self.view_height as isize)),
            KeyCode::PageDown => self.buffer.vertical(self.view_height as isize),
            KeyCode::Home => self.buffer.home(),
            KeyCode::End => self.buffer.end(),
            KeyCode::Char(ch) if !ctrl => {
                // The kitty keyboard protocol can report `~` as Shift+`.
                let ch = if ch == '`' && key.modifiers.contains(KeyModifiers::SHIFT) { '~' } else { ch };
                self.buffer.insert(&ch.to_string());
                self.mark_edited();
            }
            _ => {}
        }
        false
    }

    /// Moves the cursor to the start of the next or previous summary block, scrolling so the
    /// block is in view.
    fn jump(&mut self, forward: bool) {
        let Some(summary) = &self.summary else { return };
        let line_count = self.buffer.lines.len();
        let Some(item) = neighbour(&summary.items, self.buffer.row + 1, forward, line_count) else { return };
        let (start, end) = (item.start - 1, item.end.min(line_count) - 1);
        self.buffer.row = start;
        self.buffer.col = 0;
        self.buffer.home();
        if start < self.scroll || end >= self.scroll + self.view_height {
            self.scroll = start.saturating_sub(JUMP_MARGIN);
        }
    }

    /// Rows of the summary pane, one per source line.
    fn rows(&self) -> Vec<Row> {
        self.summary.as_ref().map_or_else(Vec::new, |summary| summary_rows(&summary.items, self.buffer.lines.len()))
    }

    /// Moves to the previous or next line of the summary pane that has text, skipping blank rows.
    fn step_line(&mut self, forward: bool) {
        if let Some(row) = neighbour_row(&self.rows(), self.buffer.row, forward) {
            self.go_to_line(row);
        }
    }

    /// On entering summary mode from a line with no summary text, selects the heading of the
    /// block the line is in, or else the next line that has text.
    fn snap_to_summary_line(&mut self) {
        let rows = self.rows();
        if rows.get(self.buffer.row).is_none_or(|row| *row != Row::Blank) {
            return;
        }
        let line = self.buffer.row + 1;
        let head = self.summary.as_ref().and_then(|s| s.items.iter().rev().find(|item| item.start <= line && line <= item.end)).map(|item| item.start - 1);
        if let Some(row) = head.or_else(|| neighbour_row(&rows, self.buffer.row, true)).or_else(|| neighbour_row(&rows, self.buffer.row, false)) {
            self.go_to_line(row);
        }
    }

    fn go_to_line(&mut self, row: usize) {
        self.buffer.row = row;
        self.buffer.col = 0;
        self.buffer.home();
    }

    /// The summary block the cursor is in, while the summary pane has focus.
    fn selected_item(&self) -> Option<&Item> {
        if self.focus != Focus::Summary {
            return None;
        }
        let line = self.buffer.row + 1;
        self.summary.as_ref()?.items.iter().rev().find(|item| item.start <= line)
    }

    fn handle_picker_key(&mut self, key: KeyEvent) {
        let Some(picker) = &mut self.picker else { return };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.picker = None,
            KeyCode::Char('c' | 'o') if ctrl => self.picker = None,
            KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Down => picker.selected += 1,
            KeyCode::Backspace => {
                picker.query.pop();
                picker.selected = 0;
            }
            KeyCode::Char(ch) if !ctrl => {
                picker.query.push(ch);
                picker.selected = 0;
            }
            KeyCode::Enter => {
                let matches = picker.matches();
                let Some(chosen) = matches.get(picker.selected.min(matches.len().saturating_sub(1))).map(|s| s.to_string()) else {
                    return;
                };
                self.picker = None;
                if let Err(error) = self.open(&chosen) {
                    self.message = Some(error);
                }
            }
            _ => {}
        }
    }

    fn save(&mut self) {
        let text = self.buffer.text();
        match std::fs::write(&self.path, &text) {
            Ok(()) => {
                self.edited = false;
                self.message = Some(format!("Saved {} ({} lines).", self.relative, self.buffer.lines.len()));
                if !self.summary_current {
                    self.start_summary(false);
                }
            }
            Err(error) => self.message = Some(format!("Could not save {}: {error}", self.relative)),
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let overview = self.summary.as_ref().map(|s| s.overview.trim()).filter(|o| !o.is_empty()).map(str::to_string);
        let overview_height = overview
            .as_deref()
            .map(|text| wrapped_height(text, area.width.saturating_sub(2) as usize).min(OVERVIEW_MAX_LINES) as u16 + 2)
            .unwrap_or(0);
        let [top, main, bottom] = Layout::vertical([Constraint::Length(overview_height), Constraint::Min(3), Constraint::Length(1)]).areas(area);
        if let Some(text) = overview {
            let block = Block::bordered().title(" overview ").border_style(dim());
            frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: true }).block(block), top);
        }
        let [left, right] = Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(main);
        self.draw_source(frame, left);
        self.draw_summary(frame, right);

        let footer = match &self.message {
            Some(message) => Line::from(Span::styled(message.clone(), Style::new().fg(Color::Yellow))),
            None if self.focus == Focus::Summary => Line::from(Span::styled(SUMMARY_HELP, dim())),
            None => {
                let hint = self.summary.as_ref().zip(self.summarizer.as_ref()).and_then(|(s, z)| z.context_hint(s));
                Line::from(Span::styled(hint.map_or(HELP.to_string(), |h| format!("{HELP} · {h}")), dim()))
            }
        };
        frame.render_widget(Paragraph::new(footer), bottom);
        if self.picker.is_some() {
            self.draw_picker(frame, area);
        }
    }

    fn draw_source(&mut self, frame: &mut Frame, area: Rect) {
        let title = format!(" {}{} ", self.relative, if self.edited { " ●" } else { "" });
        let border = if self.focus == Focus::Source { Style::new() } else { dim() };
        let block = Block::bordered().title(title).border_style(border);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let height = (inner.height as usize).max(1);
        self.view_height = height;
        let gutter = self.buffer.lines.len().to_string().len() + 1;
        let width = (inner.width as usize).saturating_sub(gutter).max(1);
        let (row, col) = (self.buffer.row, self.buffer.col);
        let selected = self.selected_item().map(|item| item.start..=item.end);
        // Keep the cursor in view.
        if row < self.scroll {
            self.scroll = row;
        } else if row >= self.scroll + height {
            self.scroll = row + 1 - height;
        }
        if col < self.hscroll {
            self.hscroll = col;
        } else if col >= self.hscroll + width {
            self.hscroll = col + 1 - width;
        }
        let lines: Vec<Line> = self
            .buffer
            .lines
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(height)
            .map(|(index, line)| {
                // Tabs show as one space so screen columns match character columns.
                let visible: String = line.chars().skip(self.hscroll).take(width).map(|c| if c == '\t' { ' ' } else { c }).collect();
                // The selected block's line numbers stand out, marking the lines it covers.
                let in_block = selected.as_ref().is_some_and(|lines| lines.contains(&(index + 1)));
                let (number, text) = match (self.focus == Focus::Summary && index == row, in_block) {
                    (true, _) => (selection().add_modifier(Modifier::REVERSED), Style::new().add_modifier(Modifier::BOLD)),
                    (false, true) => (selection(), Style::new()),
                    (false, false) => (dim(), Style::new()),
                };
                Line::from(vec![Span::styled(format!("{:>w$} ", index + 1, w = gutter - 1), number), Span::styled(visible, text)])
            })
            .collect();
        frame.render_widget(Paragraph::new(lines), inner);
        if self.picker.is_none() && self.focus == Focus::Source {
            let x = inner.x + (gutter + col - self.hscroll) as u16;
            let y = inner.y + (row - self.scroll) as u16;
            frame.set_cursor_position((x, y));
        }
    }

    fn draw_summary(&self, frame: &mut Frame, area: Rect) {
        let model = self.summarizer.as_ref().map_or("", |s| s.model.as_str());
        let state = match &self.status {
            Status::Idle => String::new(),
            Status::Unavailable(_) => "no model".into(),
            Status::Cached => "cached".into(),
            Status::Working { started, progress } => format!("summarizing… {progress} · {}s", started.elapsed().as_secs()),
            Status::Done(elapsed) => format!("{:.1}s", elapsed.as_secs_f64()),
            Status::Failed(_) => "failed".into(),
        };
        let mut title = [" summary", model, state.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" · ");
        if self.summary.is_some() && !self.summary_current {
            title.push_str(" · edited, saving updates it");
        }
        title.push(' ');
        let border = if self.focus == Focus::Summary { Style::new().fg(Color::Cyan) } else { dim() };
        let block = Block::bordered().title(title).border_style(border);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let Some(summary) = &self.summary else {
            let text = match &self.status {
                Status::Unavailable(error) => format!("No summaries: {error}\n\nPick a model with /model in chat, or `forgeflow use <id>`."),
                Status::Failed(error) => format!("Summary failed: {error}\n\nCtrl+R tries again."),
                _ => "Summarizing…".into(),
            };
            frame.render_widget(Paragraph::new(text).style(dim()).wrap(Wrap { trim: true }), inner);
            return;
        };
        let rows = summary_rows(&summary.items, self.buffer.lines.len());
        // The selected summary line is the one beside the cursor.
        let selected_row = (self.focus == Focus::Summary).then_some(self.buffer.row);
        let lines: Vec<Line> = rows
            .into_iter()
            .enumerate()
            .skip(self.scroll)
            .take(inner.height as usize)
            .map(|(index, row)| match row {
                row if Some(index) == selected_row && row != Row::Blank => {
                    let text = match row {
                        Row::Head(text) => text,
                        Row::Step(text) => format!("  {text}"),
                        Row::More(count) => format!("  … {count} more"),
                        Row::Blank => String::new(),
                    };
                    Line::from(Span::styled(text, selection().add_modifier(Modifier::REVERSED)))
                }
                Row::Head(text) => Line::from(Span::styled(text, Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
                Row::Step(text) => Line::from(Span::raw(format!("  {text}"))),
                Row::More(count) => Line::from(Span::styled(format!("  … {count} more"), dim())),
                Row::Blank => Line::default(),
            })
            .collect();
        frame.render_widget(Paragraph::new(lines), inner);
        if let Status::Failed(error) = &self.status {
            // Keep the previous summary visible and say why it was not replaced.
            let line = Rect { y: inner.y + inner.height.saturating_sub(1), height: 1.min(inner.height), ..inner };
            frame.render_widget(Clear, line);
            frame.render_widget(Paragraph::new(format!("update failed: {error}")).style(Style::new().fg(Color::Red)), line);
        }
    }

    fn draw_picker(&self, frame: &mut Frame, area: Rect) {
        let Some(picker) = &self.picker else { return };
        let popup = centered(area, 70, 60);
        frame.render_widget(Clear, popup);
        let block = Block::bordered().title(format!(" open: {}▏", picker.query)).title_bottom(" Enter opens · Esc closes ");
        let inner = block.inner(popup);
        frame.render_widget(block, popup);
        let matches = picker.matches();
        let height = inner.height as usize;
        let selected = picker.selected.min(matches.len().saturating_sub(1));
        let first = (selected + 1).saturating_sub(height);
        let lines: Vec<Line> = matches
            .iter()
            .enumerate()
            .skip(first)
            .take(height)
            .map(|(index, path)| {
                let style = if index == selected { Style::new().add_modifier(Modifier::REVERSED) } else { Style::new() };
                Line::from(Span::styled(path.to_string(), style))
            })
            .collect();
        frame.render_widget(Paragraph::new(lines), inner);
    }
}

#[derive(Debug, PartialEq)]
enum Row {
    Head(String),
    Step(String),
    More(usize),
    Blank,
}

/// One row per source line: each item's head on its first line and its steps below, up to
/// the next item. Steps that do not fit collapse into a "more" row.
fn summary_rows(items: &[Item], line_count: usize) -> Vec<Row> {
    let mut rows: Vec<Row> = (0..line_count).map(|_| Row::Blank).collect();
    for (index, item) in items.iter().enumerate() {
        let start = item.start.saturating_sub(1);
        if start >= line_count {
            break;
        }
        let limit = items.get(index + 1).map_or(line_count, |next| next.start.saturating_sub(1).min(line_count));
        rows[start] = Row::Head(item.head.clone());
        let room = limit.saturating_sub(start + 1);
        let shown = if item.body.len() > room { room.saturating_sub(1) } else { item.body.len() };
        for (offset, step) in item.body.iter().take(shown).enumerate() {
            rows[start + 1 + offset] = Row::Step(step.clone());
        }
        if item.body.len() > shown && room > 0 {
            rows[start + 1 + shown] = Row::More(item.body.len() - shown);
        }
    }
    rows
}

fn wrapped_height(text: &str, width: usize) -> usize {
    let width = width.max(1);
    text.lines().map(|line| line.chars().count().div_ceil(width).max(1)).sum()
}

fn centered(area: Rect, width_percent: u16, height_percent: u16) -> Rect {
    let width = area.width * width_percent / 100;
    let height = area.height * height_percent / 100;
    Rect { x: area.x + (area.width - width) / 2, y: area.y + (area.height - height) / 2, width, height }
}

fn dim() -> Style {
    Style::new().fg(Color::DarkGray)
}

fn selection() -> Style {
    Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
}

/// The nearest row before or after `from` that has summary text.
fn neighbour_row(rows: &[Row], from: usize, forward: bool) -> Option<usize> {
    let has_text = |index: &usize| rows[*index] != Row::Blank;
    if forward { (from + 1..rows.len()).find(has_text) } else { (0..from.min(rows.len())).rev().find(has_text) }
}

/// The next block starting after `line`, or the last one starting before it (1-based),
/// ignoring blocks past the end of an edited, shorter file.
fn neighbour(items: &[Item], line: usize, forward: bool, line_count: usize) -> Option<&Item> {
    let mut items = items.iter().filter(|item| item.start >= 1 && item.start <= line_count);
    if forward { items.find(|item| item.start > line) } else { items.rev().find(|item| item.start < line) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(start: usize, head: &str, body: &[&str]) -> Item {
        Item { start, end: start, head: head.into(), body: body.iter().map(|s| s.to_string()).collect() }
    }

    #[test]
    fn buffer_round_trips_line_endings() {
        for text in ["", "a", "a\n", "a\nb\n", "\n", "a\r\nb\r\n"] {
            let expected = if text.is_empty() { "\n" } else { text };
            assert_eq!(Buffer::new(text).text(), expected, "{text:?}");
        }
    }

    #[test]
    fn editing_splits_joins_and_keeps_indentation() {
        let mut buffer = Buffer::new("    let x = 1;\n");
        buffer.end();
        buffer.newline();
        buffer.insert("y");
        assert_eq!(buffer.text(), "    let x = 1;\n    y\n");
        buffer.home();
        buffer.home();
        buffer.backspace();
        assert_eq!(buffer.text(), "    let x = 1;    y\n");
        buffer.delete();
        buffer.insert("é");
        buffer.left(false);
        buffer.delete();
        assert_eq!(buffer.text(), "    let x = 1;   y\n");
    }

    #[test]
    fn vertical_moves_keep_the_goal_column() {
        let mut buffer = Buffer::new("abcdef\nab\nabcdef\n");
        buffer.end();
        buffer.vertical(1);
        assert_eq!((buffer.row, buffer.col), (1, 2));
        buffer.vertical(1);
        assert_eq!((buffer.row, buffer.col), (2, 6));
        buffer.vertical(100);
        assert_eq!(buffer.row, 2);
    }

    #[test]
    fn word_moves_skip_punctuation_then_a_word() {
        let mut buffer = Buffer::new("fs::create_dir_all(folder)");
        buffer.end();
        buffer.left(true);
        assert_eq!(buffer.col, 19);
        buffer.left(true);
        assert_eq!(buffer.col, 4);
        buffer.right(true);
        assert_eq!(buffer.col, 18);
    }

    #[test]
    fn summary_rows_line_up_with_source_lines() {
        let items = [item(2, "a()", &["one", "two", "three"]), item(5, "b()", &["x"])];
        assert_eq!(
            summary_rows(&items, 6),
            vec![
                Row::Blank,
                Row::Head("a()".into()),
                Row::Step("one".into()),
                Row::More(2),
                Row::Head("b()".into()),
                Row::Step("x".into()),
            ]
        );
        // Items past the end of an edited, shorter file are skipped.
        assert_eq!(summary_rows(&items, 3).len(), 3);
    }

    #[test]
    fn jumps_go_to_the_neighbouring_block_starts() {
        let items = [item(2, "a", &[]), item(5, "b", &[]), item(9, "c", &[])];
        let start = |found: Option<&Item>| found.map(|item| item.start);
        assert_eq!(start(neighbour(&items, 1, true, 10)), Some(2));
        assert_eq!(start(neighbour(&items, 3, true, 10)), Some(5));
        assert_eq!(start(neighbour(&items, 5, false, 10)), Some(2));
        assert_eq!(start(neighbour(&items, 2, false, 10)), None);
        assert_eq!(start(neighbour(&items, 9, true, 10)), None);
        // A block past the end of the file is skipped.
        assert_eq!(start(neighbour(&items, 5, true, 8)), None);
    }

    #[test]
    fn line_steps_skip_blank_summary_rows() {
        let rows = [Row::Head("a".into()), Row::Blank, Row::Step("x".into()), Row::Blank, Row::Blank, Row::Head("b".into())];
        assert_eq!(neighbour_row(&rows, 0, true), Some(2));
        assert_eq!(neighbour_row(&rows, 2, true), Some(5));
        assert_eq!(neighbour_row(&rows, 5, true), None);
        assert_eq!(neighbour_row(&rows, 5, false), Some(2));
        assert_eq!(neighbour_row(&rows, 4, false), Some(2));
        assert_eq!(neighbour_row(&rows, 0, false), None);
    }

    #[test]
    fn picker_matches_in_order_and_prefers_file_names() {
        let picker = Picker { query: "sum".into(), files: vec!["examples/usage.md".into(), "src/main.rs".into(), "docs/summary-notes.md".into(), "src/summary.rs".into()], selected: 0 };
        assert_eq!(picker.matches(), ["src/summary.rs", "docs/summary-notes.md", "examples/usage.md"]);
        assert!(!is_subsequence("zz", "src/summary.rs"));
    }
}

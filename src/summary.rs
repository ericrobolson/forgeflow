//! Line-anchored pseudocode summaries of a file, cached by content in `_forgeflow/summaries`.
//!
//! A file that fits the context budget is summarized in one request; a longer one is cut
//! into overlapping windows by token count and summarized in order, each request carrying
//! only the names found so far. Every request starts from a fresh context.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};

use crate::{
    chat::{self, ChatBackend, Delta, OpenAiCompatible, StepOptions},
    config::Config,
    device::Device,
    models,
    project::SERVER_LOG_FILE,
    server::LocalServer,
    workflow::Context,
};

/// Bump when the prompt or output format changes, so cached summaries are regenerated.
const PROMPT_VERSION: &str = "3";
/// Tokens set aside for the instructions and the names carried between windows.
const PROMPT_TOKENS: u64 = 700;
/// Tokens kept free so an underestimated count never overflows the slot.
const MARGIN_TOKENS: u64 = 256;
const MIN_WINDOW_TOKENS: u64 = 512;
/// Lines shared by neighbouring windows, so an item cut at one edge is seen whole by the next.
const OVERLAP_LINES: usize = 20;
/// Longer lines (minified code, one-line JSON) are cut in the prompt.
const MAX_LINE_CHARS: usize = 800;
/// Characters of earlier names carried into the next window.
const CARRIED_NAMES_CHARS: usize = 1500;
/// Used when the server cannot count tokens; dense code runs near three characters a token.
const FALLBACK_CHARS_PER_TOKEN: f64 = 3.0;

/// One summarized unit, such as a function or a section, covering lines `start..=end` (1-based).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub start: usize,
    pub end: usize,
    pub head: String,
    #[serde(default)]
    pub body: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub model: String,
    pub overview: String,
    pub items: Vec<Item>,
    /// How many windows the file was summarized in; 1 means the model saw it whole.
    pub windows: usize,
}

/// Reported while a summary is generated.
#[derive(Debug, Clone, PartialEq)]
pub enum Progress {
    Window { index: usize, total: usize },
    /// Tokens received so far for the current request.
    Tokens(usize),
    Overview,
}

/// The selected model and the settings summaries depend on. Creating one starts nothing, so
/// cached summaries show without waiting for a model to load.
#[derive(Clone)]
pub struct Summarizer {
    /// The model name, part of every cache key.
    pub model: String,
    pub ctx: u64,
    pub cache_dir: PathBuf,
    /// Whether the server is llama-server, which can count tokens exactly.
    pub exact_tokens: bool,
    /// The largest context this machine holds comfortably for the model, when it is catalogued.
    pub max_ctx: Option<u64>,
    /// The model server, once one is known to be running.
    pub backend: Option<OpenAiCompatible>,
    /// How to start llama-server when none is running yet.
    launch: Option<Launch>,
}

#[derive(Clone)]
struct Launch {
    model_path: PathBuf,
    port: u16,
    log: PathBuf,
}

impl Summarizer {
    pub fn new(context: &Context) -> Result<Self, String> {
        let config = Config::load(&context.project.folder())?;
        let local = config.endpoint.is_none();
        let max_ctx = config
            .model
            .as_deref()
            .filter(|_| local)
            .and_then(models::find)
            .map(|model| models::max_ctx(&model, &Device::probe()));
        let mut summarizer = Self {
            model: String::new(),
            ctx: config.ctx,
            cache_dir: context.project.summaries_folder(),
            exact_tokens: local,
            max_ctx,
            backend: None,
            launch: None,
        };
        if let Some(endpoint) = &config.endpoint {
            // The same model name `chat::connect` sends to the endpoint.
            summarizer.model = config.endpoint_model.clone().unwrap_or_else(|| "default".into());
            summarizer.backend = Some(OpenAiCompatible { endpoint: endpoint.clone(), model: summarizer.model.clone() });
            return Ok(summarizer);
        }
        let id = config.model.ok_or("no model selected; pick one with /model in chat or `forgeflow use <id>`")?;
        let (model_path, _) = models::resolve_installed(&id)?;
        summarizer.model = id.clone();
        match &context.server {
            Some(server) if server.model_path == model_path => {
                summarizer.backend = Some(OpenAiCompatible { endpoint: server.endpoint(), model: id });
            }
            _ => {
                let log = context.project.folder().join(SERVER_LOG_FILE);
                summarizer.launch = Some(Launch { model_path, port: config.port, log });
            }
        }
        Ok(summarizer)
    }

    /// The running server, or a newly started one, which the caller must keep alive.
    /// Prints nothing, so it can run while the editor owns the screen.
    pub fn backend_or_start(&self) -> Result<(OpenAiCompatible, Option<LocalServer>), String> {
        if let Some(backend) = &self.backend {
            return Ok((backend.clone(), None));
        }
        let launch = self.launch.as_ref().ok_or("no model server is configured")?;
        let server = LocalServer::start_quietly(&launch.model_path, launch.port, self.ctx, &launch.log)?;
        let backend = OpenAiCompatible { endpoint: server.endpoint(), model: self.model.clone() };
        Ok((backend, Some(server)))
    }

    pub fn cached(&self, text: &str) -> Option<Summary> {
        load_cached(&self.cache_dir, &cache_key(&self.model, text))
    }

    /// The cached summary of `text`, or a new one (stored for next time). `force` skips the
    /// cache. Generating needs `backend` set.
    pub fn summarize(&self, label: &str, text: &str, force: bool, on_progress: &mut dyn FnMut(Progress)) -> Result<Summary, String> {
        let key = cache_key(&self.model, text);
        if !force && let Some(summary) = load_cached(&self.cache_dir, &key) {
            return Ok(summary);
        }
        let backend = self.backend.as_ref().ok_or("the model server has not been started")?;
        let endpoint = backend.endpoint.clone();
        let exact = self.exact_tokens;
        let count = move |text: &str| if exact { count_tokens(&endpoint, text) } else { None };
        let summary = generate(backend, &self.model, label, text, self.ctx, &count, on_progress)?;
        store(&self.cache_dir, &key, &summary)?;
        Ok(summary)
    }

    /// A note suggesting a larger `ctx` when this machine could summarize the file in one pass.
    pub fn context_hint(&self, summary: &Summary) -> Option<String> {
        let max = self.max_ctx?;
        (summary.windows > 1 && max > self.ctx).then(|| {
            format!("{} windows · this machine fits ctx {max} for {}; set \"ctx\" in _forgeflow/config.json to see more at once", summary.windows, self.model)
        })
    }
}

/// The cache file name: the content hash together with everything else the summary depends on.
pub fn cache_key(model: &str, text: &str) -> String {
    let mut hasher = Sha256::new();
    // Length prefixes keep ("ab", "c") and ("a", "bc") apart.
    for part in [PROMPT_VERSION, model, text] {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn load_cached(folder: &std::path::Path, key: &str) -> Option<Summary> {
    let text = std::fs::read_to_string(folder.join(format!("{key}.json"))).ok()?;
    serde_json::from_str(&text).ok()
}

fn store(folder: &std::path::Path, key: &str, summary: &Summary) -> Result<(), String> {
    crate::project::Project::create_ignored_folder(folder).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(summary).map_err(|e| e.to_string())?;
    // Write then rename, so a reader never sees half a file.
    let temporary = folder.join(format!("{key}.json.{}.tmp", std::process::id()));
    std::fs::write(&temporary, json).map_err(|e| e.to_string())?;
    std::fs::rename(&temporary, folder.join(format!("{key}.json"))).map_err(|e| e.to_string())
}

/// Exact token count from llama-server's `/tokenize`; `None` when the server has no such endpoint.
pub fn count_tokens(endpoint: &str, text: &str) -> Option<u64> {
    let root = endpoint.trim_end_matches('/').trim_end_matches("/v1");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .into();
    let mut response = agent.post(&format!("{root}/tokenize")).send_json(json!({"content": text})).ok()?;
    let body: Json = response.body_mut().read_json().ok()?;
    Some(body["tokens"].as_array()?.len() as u64)
}

/// Input tokens per request: half the slot's context less the prompt. Small models do worse
/// well before their context is full, and the other half leaves room for the reply.
pub fn input_budget(ctx: u64) -> u64 {
    (ctx / 2).saturating_sub(PROMPT_TOKENS).max(MIN_WINDOW_TOKENS)
}

fn output_budget(ctx: u64, input: u64) -> u32 {
    ctx.saturating_sub(input + PROMPT_TOKENS + MARGIN_TOKENS).max(256) as u32
}

/// Lines numbered for the prompt (`12\tcode`), with very long lines cut.
fn numbered_lines(text: &str) -> Vec<String> {
    text.lines()
        .enumerate()
        .map(|(i, line)| {
            if line.chars().count() > MAX_LINE_CHARS {
                let cut: String = line.chars().take(MAX_LINE_CHARS).collect();
                format!("{}\t{cut}…", i + 1)
            } else {
                format!("{}\t{line}", i + 1)
            }
        })
        .collect()
}

/// Splits lines into windows of at most `budget` tokens, cut at line ends, with neighbouring
/// windows sharing up to `OVERLAP_LINES` lines. Ranges are 0-based and end-exclusive.
fn windows(line_tokens: &[u64], budget: u64) -> Vec<std::ops::Range<usize>> {
    let mut windows = vec![];
    let mut start = 0;
    while start < line_tokens.len() {
        let mut end = start;
        let mut used = 0;
        // A window always takes at least one line.
        while end < line_tokens.len() && (end == start || used + line_tokens[end] <= budget) {
            used += line_tokens[end];
            end += 1;
        }
        windows.push(start..end);
        if end == line_tokens.len() {
            break;
        }
        let overlap = OVERLAP_LINES.min((end - start) / 4);
        start = end - overlap;
    }
    windows
}

fn generate(
    backend: &dyn ChatBackend,
    model: &str,
    label: &str,
    text: &str,
    ctx: u64,
    count_tokens: &dyn Fn(&str) -> Option<u64>,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<Summary, String> {
    let lines = numbered_lines(text);
    if text.trim().is_empty() {
        return Ok(Summary { model: model.into(), overview: "(empty file)".into(), items: vec![], windows: 0 });
    }
    let joined = lines.join("\n");
    let chars = joined.chars().count().max(1);
    let tokens_per_char = match count_tokens(&joined) {
        Some(tokens) => tokens as f64 / chars as f64,
        None => 1.0 / FALLBACK_CHARS_PER_TOKEN,
    };
    let line_tokens: Vec<u64> = lines
        .iter()
        .map(|line| ((line.chars().count() + 1) as f64 * tokens_per_char).ceil() as u64)
        .collect();
    let budget = input_budget(ctx);
    let max_tokens = output_budget(ctx, budget);
    let windows = windows(&line_tokens, budget);
    let total = windows.len();

    let mut items: Vec<Item> = vec![];
    let mut overviews = vec![];
    for (index, range) in windows.iter().enumerate() {
        on_progress(Progress::Window { index, total });
        let (first, last) = (range.start + 1, range.end);
        let part = (total > 1).then(|| Part { index, total, first, last, earlier: carried_names(&items) });
        let body = lines[range.clone()].join("\n");
        let (overview, window_items) = summarize_window(backend, label, &body, part.as_ref(), first..=last, text, max_tokens, on_progress)?;
        overviews.push(overview);
        // The later window saw the overlap whole, so it replaces what the earlier one said there.
        items.retain(|item| item.start < first);
        for item in window_items {
            if items.last().is_none_or(|previous| item.start > previous.end) {
                items.push(item);
            }
        }
    }

    let overview = if total == 1 {
        overviews.remove(0)
    } else {
        on_progress(Progress::Overview);
        combine_overviews(backend, label, &overviews, &items).unwrap_or_else(|_| overviews.join(" "))
    };
    Ok(Summary { model: model.into(), overview, items, windows: total })
}

struct Part {
    index: usize,
    total: usize,
    first: usize,
    last: usize,
    earlier: String,
}

/// The most recent item heads, newest kept, within `CARRIED_NAMES_CHARS`.
fn carried_names(items: &[Item]) -> String {
    let mut kept = vec![];
    let mut used = 0;
    for item in items.iter().rev() {
        used += item.head.len() + 1;
        if used > CARRIED_NAMES_CHARS {
            break;
        }
        kept.push(item.head.as_str());
    }
    kept.reverse();
    kept.join("\n")
}

const SYSTEM_PROMPT: &str = "You summarize a file, which may be source code in any language, a configuration file, or a document, as high-level pseudocode shown beside it in an editor. Each line of the file is numbered.\n\
Reply with JSON only: {\"overview\": \"...\", \"items\": [{\"start\": 1, \"end\": 9, \"head\": \"...\", \"body\": [\"...\"]}]}\n\
- overview: one or two sentences on what the file is for.\n\
- items: one per meaningful unit in order, such as a function, type, impl block, module, config section, heading or group of paragraphs. start and end are the first and last line numbers it covers; items do not overlap. A run of imports can be one item. Skip blank lines and trivial code.\n\
- head: one short line naming the unit, signature style, such as `Project::initialize(path) -> Project` or `def handle_call(:get, state)` or `§3 Leader election`.\n\
- body: short pseudocode steps, fewer than the lines the item covers, such as `fs::create_dir_all(folder)` or `for each entry: ignore_entries(folder, entries)`. Use the exact module, type, function and variable names from the file and never invent names. For prose, give the claims, definitions and results in the document's own terms.\n\
- Tests: one item per test case, whose head is `test ` and the test's name, such as `test saves_and_reads_back` or `test \"returns nil for a missing key\"`, with an empty body. Do not describe what a test does. A test module's imports, setup and helpers are one item.\n\
- Keep every line under 70 characters.\n\
\n\
Example. For this file:\n\
1\tuse std::fs;\n\
2\t\n\
3\tpub struct Store { root: PathBuf }\n\
4\t\n\
5\timpl Store {\n\
6\t    pub fn save(&self, name: &str, text: &str) -> io::Result<()> {\n\
7\t        let path = self.root.join(name);\n\
8\t        fs::create_dir_all(&self.root)?;\n\
9\t        fs::write(path, text)\n\
10\t    }\n\
11\t}\n\
12\t\n\
13\t#[test]\n\
14\tfn saves_and_reads_back() {\n\
15\t    let store = Store { root: temp_dir() };\n\
16\t    store.save(\"a\", \"hi\").unwrap();\n\
17\t}\n\
the reply is:\n\
{\"overview\": \"Store writes named text files below a root folder.\", \"items\": [{\"start\": 1, \"end\": 1, \"head\": \"use std::fs\", \"body\": []}, {\"start\": 3, \"end\": 3, \"head\": \"struct Store { root: PathBuf }\", \"body\": []}, {\"start\": 6, \"end\": 10, \"head\": \"Store::save(name, text) -> io::Result\", \"body\": [\"path = root.join(name)\", \"fs::create_dir_all(root)\", \"fs::write(path, text)\"]}, {\"start\": 13, \"end\": 17, \"head\": \"test saves_and_reads_back\", \"body\": []}]}\n\
Heads name the real function or type, not a description like \"Saving files\".";

/// Restricts output to the reply's JSON shape on llama-server; other servers ignore it.
const SUMMARY_GRAMMAR: &str = r#"root ::= "{" ws "\"overview\":" ws string "," ws "\"items\":" ws "[" ws ( item ( "," ws item )* )? ws "]" ws "}"
item ::= "{" ws "\"start\":" ws int "," ws "\"end\":" ws int "," ws "\"head\":" ws string "," ws "\"body\":" ws "[" ws ( string ( "," ws string )* )? ws "]" ws "}"
string ::= "\"" ( [^"\\\x7F\x00-\x1F] | "\\" ( ["\\/bfnrt] | "u" [0-9a-fA-F]{4} ) )* "\""
int ::= [0-9]{1,7}
ws ::= [ \t\n]{0,8}"#;

#[derive(Deserialize)]
struct Reply {
    #[serde(default)]
    overview: String,
    #[serde(default)]
    items: Vec<Item>,
}

#[allow(clippy::too_many_arguments)]
fn summarize_window(
    backend: &dyn ChatBackend,
    label: &str,
    numbered: &str,
    part: Option<&Part>,
    lines: std::ops::RangeInclusive<usize>,
    source: &str,
    max_tokens: u32,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<(String, Vec<Item>), String> {
    let intro = match part {
        None => format!("File: {label}"),
        Some(part) => {
            let earlier = if part.earlier.is_empty() {
                String::new()
            } else {
                format!("\nUnits already summarized from earlier parts:\n{}\n", part.earlier)
            };
            format!(
                "File: {label}, part {} of {} (lines {}-{}). Summarize only these lines; the overview covers just this part.{earlier}",
                part.index + 1, part.total, part.first, part.last
            )
        }
    };
    let mut messages = vec![
        json!({"role": "system", "content": SYSTEM_PROMPT}),
        json!({"role": "user", "content": format!("{intro}\n\n{numbered}")}),
    ];
    let options = StepOptions {
        grammar: Some(SUMMARY_GRAMMAR.into()),
        temperature: Some(0.0),
        max_tokens: Some(max_tokens),
        no_thinking: true,
        slot: Some(crate::server::ANSWER_SLOT),
    };
    // One correction attempt, for unparseable JSON or names that are not in the file.
    for attempt in 0..2 {
        let mut received = 0;
        let turn = backend.step(&messages, &[], &options, &mut |delta| {
            if let Delta::Text(_) = delta {
                received += 1;
                on_progress(Progress::Tokens(received));
            }
        })?;
        let correction = match parse_reply(&turn.content) {
            Err(error) if attempt == 0 => format!("That reply was not valid JSON ({error}). Reply with the JSON object only."),
            Err(error) => return Err(format!("the model's summary was not valid JSON: {error}")),
            Ok(reply) => {
                let items = clean_items(reply.items, &lines);
                let unknown = unknown_names(&items, &reply.overview, source);
                if unknown.is_empty() || attempt == 1 {
                    return Ok((reply.overview.trim().to_string(), drop_unknown_steps(items, source)));
                }
                format!(
                    "These names are not in the file: {}. Use only names that appear in the file, and reply with the corrected JSON.",
                    unknown.join(", ")
                )
            }
        };
        messages.push(json!({"role": "assistant", "content": turn.content}));
        messages.push(json!({"role": "user", "content": correction}));
    }
    unreachable!("the second attempt always returns")
}

/// Parses the reply, tolerating a code fence or text around the JSON object.
fn parse_reply(content: &str) -> Result<Reply, String> {
    let start = content.find('{').ok_or("no JSON object")?;
    let end = content.rfind('}').ok_or("no JSON object")?;
    if end < start {
        return Err("no JSON object".into());
    }
    serde_json::from_str(&content[start..=end]).map_err(|e| e.to_string())
}

/// Keeps items inside the window, in order and without overlaps; ends past the window are clamped.
fn clean_items(mut items: Vec<Item>, lines: &std::ops::RangeInclusive<usize>) -> Vec<Item> {
    items.retain(|item| lines.contains(&item.start) && item.end >= item.start && !item.head.trim().is_empty());
    items.sort_by_key(|item| item.start);
    let mut cleaned: Vec<Item> = vec![];
    for mut item in items {
        item.end = item.end.min(*lines.end());
        item.head = strip_ticks(&item.head);
        item.body = item.body.iter().map(|step| strip_ticks(step)).filter(|step| !step.is_empty()).collect();
        // Tests are listed by name only.
        if item.head.starts_with("test ") {
            item.body.clear();
        }
        if cleaned.last().is_none_or(|previous| item.start > previous.end) {
            cleaned.push(item);
        }
    }
    cleaned
}

/// Removes backticks and keeps each head or step on one line: a decoded `'\n'` literal
/// shows escaped, as it is written in the source.
fn strip_ticks(text: &str) -> String {
    text.replace('`', "").replace('\n', "\\n").replace('\r', "\\r").replace('\t', " ").trim().to_string()
}

/// Identifiers in code-looking tokens (`a::b`, `a.b(`, `snake_case`, `camelCase`) that never
/// appear in the source. Plain words are not checked, so pseudocode can still say "for each".
fn unknown_names(items: &[Item], overview: &str, source: &str) -> Vec<String> {
    let mut unknown: Vec<String> = vec![];
    let text = items
        .iter()
        .flat_map(|item| std::iter::once(item.head.as_str()).chain(item.body.iter().map(String::as_str)))
        .chain(std::iter::once(overview));
    for line in text {
        for name in names_in(line) {
            if !source.contains(name.as_str()) && !unknown.contains(&name) {
                unknown.push(name);
            }
        }
    }
    unknown
}

fn names_in(line: &str) -> Vec<String> {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut names = vec![];
    for token in line.split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '"' | '\'' | '`' | '[' | ']' | '{' | '}' | '<' | '>' | '=' | '|' | '&' | '!' | '+' | '*')) {
        let token = token.trim_matches(|c: char| !is_ident(c) && c != '(');
        let looks_like_code = token.contains("::")
            || token.contains('_')
            || token.contains('(')
            || token.char_indices().any(|(i, c)| c == '.' && token[..i].ends_with(is_ident) && token[i + 1..].starts_with(is_ident))
            || token.chars().zip(token.chars().skip(1)).any(|(a, b)| a.is_lowercase() && b.is_uppercase());
        if !looks_like_code {
            continue;
        }
        for segment in token.split(|c: char| !is_ident(c)) {
            // Single letters and numbers say nothing about whether a name is real.
            if segment.chars().count() >= 2 && !segment.chars().all(|c| c.is_ascii_digit()) {
                names.push(segment.to_string());
            }
        }
    }
    names
}

/// After the correction attempt, steps naming things not in the file are dropped; heads stay
/// so the layout still lines up with the source.
fn drop_unknown_steps(mut items: Vec<Item>, source: &str) -> Vec<Item> {
    for item in &mut items {
        item.body.retain(|step| names_in(step).iter().all(|name| source.contains(name.as_str())));
    }
    items
}

fn combine_overviews(backend: &dyn ChatBackend, label: &str, overviews: &[String], items: &[Item]) -> Result<String, String> {
    let heads: Vec<&str> = items.iter().map(|item| item.head.as_str()).collect();
    let prompt = format!(
        "File: {label}. Its parts were summarized as:\n{}\n\nIts units:\n{}\n\nIn two or three sentences, say what the whole file is for, using its real names.",
        overviews.iter().enumerate().map(|(i, o)| format!("{}. {o}", i + 1)).collect::<Vec<_>>().join("\n"),
        heads.join("\n"),
    );
    let messages = [
        json!({"role": "system", "content": "Reply concisely in plain text."}),
        json!({"role": "user", "content": prompt}),
    ];
    let options = StepOptions {
        temperature: Some(0.0),
        max_tokens: Some(300),
        no_thinking: true,
        slot: Some(crate::server::ANSWER_SLOT),
        ..StepOptions::default()
    };
    Ok(backend.step(&messages, &[], &options, &mut |_| {})?.content.trim().to_string())
}

/// The summary as plain text, for the `summarize` word: the overview, then one item per block.
pub fn to_text(summary: &Summary) -> String {
    let mut out = summary.overview.clone();
    for item in &summary.items {
        out.push_str(&format!("\n\n{}-{}  {}", item.start, item.end, item.head));
        for step in &item.body {
            out.push_str(&format!("\n    {step}"));
        }
    }
    out
}

/// Summarizes a project file for the `summarize` word, reporting progress on stderr.
pub fn for_file(context: &mut Context, relative: &str) -> Result<Summary, String> {
    let path = context.project.resolve(relative)?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{relative}: {e}"))?;
    let mut summarizer = Summarizer::new(context)?;
    if let Some(summary) = summarizer.cached(&text) {
        return Ok(summary);
    }
    summarizer.backend = Some(chat::connect(context)?.0);
    let label = relative.trim();
    summarizer.summarize(label, &text, false, &mut |progress| match progress {
        Progress::Window { index, total } if total > 1 => eprintln!("summarizing {label}: part {} of {total}", index + 1),
        Progress::Window { .. } => eprintln!("summarizing {label}"),
        Progress::Overview => eprintln!("summarizing {label}: overview"),
        Progress::Tokens(_) => {}
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::Turn;
    use std::cell::RefCell;

    /// Replies with canned turns in order and records every request's messages.
    struct Scripted {
        replies: RefCell<Vec<String>>,
        requests: RefCell<Vec<Vec<Json>>>,
    }

    impl Scripted {
        fn new(replies: &[&str]) -> Self {
            Self { replies: RefCell::new(replies.iter().rev().map(|r| r.to_string()).collect()), requests: RefCell::new(vec![]) }
        }
    }

    impl ChatBackend for Scripted {
        fn step(&self, messages: &[Json], _: &[Json], _: &StepOptions, _: &mut dyn FnMut(Delta)) -> Result<Turn, String> {
            self.requests.borrow_mut().push(messages.to_vec());
            let content = self.replies.borrow_mut().pop().ok_or("no more scripted replies")?;
            Ok(Turn { content, ..Turn::default() })
        }
    }

    const SOURCE: &str = "use std::fs;\n\nfn initialize(folder: &str) {\n    fs::create_dir_all(folder);\n}\n";

    fn run(backend: &Scripted, text: &str, ctx: u64) -> Summary {
        generate(backend, "m", "src/lib.rs", text, ctx, &|_| None, &mut |_| {}).unwrap()
    }

    #[test]
    fn cache_key_changes_with_model_and_contents() {
        let key = cache_key("m", "a");
        assert_eq!(key.len(), 64);
        assert_eq!(key, cache_key("m", "a"));
        assert_ne!(key, cache_key("n", "a"));
        assert_ne!(key, cache_key("m", "b"));
        assert_ne!(cache_key("ab", "c"), cache_key("a", "bc"));
    }

    #[test]
    fn windows_respect_the_budget_and_overlap() {
        assert_eq!(windows(&[10; 5], 100), vec![0..5]);
        let cut = windows(&[10; 100], 200);
        assert_eq!(cut[0], 0..20);
        // Each window starts inside the previous one and the last reaches the end.
        for pair in cut.windows(2) {
            assert!(pair[1].start < pair[0].end && pair[1].start > pair[0].start);
        }
        assert_eq!(cut.last().unwrap().end, 100);
        // A line bigger than the budget still gets a window of its own.
        assert_eq!(windows(&[500, 10], 100), vec![0..1, 1..2]);
    }

    #[test]
    fn a_file_that_fits_is_summarized_in_one_fresh_request() {
        let backend = Scripted::new(&[r#"{"overview":"Creates folders.","items":[
            {"start":1,"end":1,"head":"imports: std::fs","body":[]},
            {"start":3,"end":5,"head":"initialize(folder)","body":["fs::create_dir_all(folder)"]}]}"#]);
        let summary = run(&backend, SOURCE, 8192);
        assert_eq!(summary.windows, 1);
        assert_eq!(summary.overview, "Creates folders.");
        assert_eq!(summary.items[1].body, ["fs::create_dir_all(folder)"]);
        let requests = backend.requests.borrow();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].len(), 2, "only the system prompt and the file");
        assert!(requests[0][1]["content"].as_str().unwrap().contains("3\tfn initialize(folder: &str) {"));
    }

    #[test]
    fn invented_names_get_one_correction_then_are_dropped() {
        let invented = r#"{"overview":"x","items":[{"start":3,"end":5,"head":"initialize(folder)","body":["fs::make_folders(folder)","return"]}]}"#;
        let backend = Scripted::new(&[invented, invented]);
        let summary = run(&backend, SOURCE, 8192);
        let requests = backend.requests.borrow();
        assert_eq!(requests.len(), 2);
        assert!(requests[1][3]["content"].as_str().unwrap().contains("make_folders"));
        assert_eq!(summary.items[0].body, ["return"]);
    }

    #[test]
    fn items_outside_the_window_or_overlapping_are_dropped() {
        let items = vec![
            Item { start: 4, end: 9, head: "b".into(), body: vec![] },
            Item { start: 1, end: 5, head: "`a`".into(), body: vec![] },
            Item { start: 20, end: 21, head: "far".into(), body: vec![] },
        ];
        let cleaned = clean_items(items, &(1..=8));
        assert_eq!(cleaned.len(), 1);
        assert_eq!((cleaned[0].start, cleaned[0].end, cleaned[0].head.as_str()), (1, 5, "a"));
        let test = vec![Item { start: 1, end: 3, head: "test reads_back".into(), body: vec!["store.save(a)".into()] }];
        assert!(clean_items(test, &(1..=3))[0].body.is_empty());
        let multiline = vec![Item { start: 1, end: 1, head: "push('\n')".into(), body: vec!["a\tb".into()] }];
        let cleaned = clean_items(multiline, &(1..=1));
        assert_eq!((cleaned[0].head.as_str(), cleaned[0].body[0].as_str()), ("push('\\n')", "a b"));
    }

    /// Answers each window request with one item covering its whole line range.
    struct PerWindow {
        requests: RefCell<Vec<Vec<Json>>>,
    }

    impl ChatBackend for PerWindow {
        fn step(&self, messages: &[Json], _: &[Json], options: &StepOptions, _: &mut dyn FnMut(Delta)) -> Result<Turn, String> {
            self.requests.borrow_mut().push(messages.to_vec());
            let prompt = messages[1]["content"].as_str().unwrap();
            let content = if options.grammar.is_none() {
                "The whole file.".to_string()
            } else {
                let range = prompt.split("(lines ").nth(1).unwrap().split(')').next().unwrap();
                let (start, end) = range.split_once('-').unwrap();
                format!(r#"{{"overview":"part","items":[{{"start":{start},"end":{end},"head":"f{start}()","body":[]}}]}}"#)
            };
            Ok(Turn { content, ..Turn::default() })
        }
    }

    #[test]
    fn long_files_are_summarized_in_windows_carrying_earlier_names() {
        let text: String = (1..=400).map(|n| format!("fn f{n}() {{ g{n}(); }}\n")).collect();
        let backend = PerWindow { requests: RefCell::new(vec![]) };
        let summary = generate(&backend, "m", "src/lib.rs", &text, 2048, &|_| None, &mut |_| {}).unwrap();
        assert!(summary.windows > 2, "{}", summary.windows);
        assert_eq!(summary.overview, "The whole file.");
        let requests = backend.requests.borrow();
        assert_eq!(requests.len(), summary.windows + 1, "one request per window plus the overview");
        assert!(requests.iter().all(|messages| messages.len() == 2), "every request starts fresh");
        let second = requests[1][1]["content"].as_str().unwrap();
        assert!(second.contains("part 2 of") && second.contains("f1()"));
        // Each later window replaces the overlap, so items never overlap and reach the end.
        assert!(summary.items.windows(2).all(|pair| pair[1].start > pair[0].end));
        assert_eq!(summary.items.last().unwrap().end, 400);
    }

    #[test]
    fn code_looking_tokens_are_checked_and_plain_words_are_not() {
        assert_eq!(names_in("for each entry: ignore_entries(folder, IGNORED)"), ["ignore_entries", "folder"]);
        assert_eq!(names_in("Project::initialize(path) -> Project"), ["Project", "initialize", "path"]);
        assert_eq!(names_in("GenServer.call reads the state"), ["GenServer", "call"]);
        assert!(names_in("returns e.g. the value").is_empty());
    }

    #[test]
    fn summaries_round_trip_through_the_cache() {
        let folder = std::env::temp_dir().join(format!("forgeflow-summaries-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let summary = Summary { model: "m".into(), overview: "o".into(), items: vec![], windows: 1 };
        store(&folder, "k", &summary).unwrap();
        assert_eq!(load_cached(&folder, "k"), Some(summary));
        assert_eq!(std::fs::read_to_string(folder.join(".gitignore")).unwrap(), "*\n");
        assert_eq!(load_cached(&folder, "missing"), None);
        std::fs::remove_dir_all(folder).unwrap();
    }
}

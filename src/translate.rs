use std::collections::HashMap;

use serde_json::{Value as Json, json};

use crate::{
    chat::{ChatBackend, StepOptions},
    ops::Ops,
    parser::{self, Source},
    registers::{REGISTER_COUNT, Registers},
    server::TRANSLATE_SLOT,
    type_::{Type, TypeKind},
    user_words::UserWord,
    words,
    workflow,
};

/// Longest program the model may write; programs are a handful of words.
const MAX_PROGRAM_TOKENS: u32 = 256;
/// History is trimmed in blocks of this many turns, so the translator sees between
/// HISTORY_BLOCK and 2 × HISTORY_BLOCK - 1 recent turns.
const HISTORY_BLOCK: usize = 8;
/// How much of a turn's output the next turn sees.
const OUTPUT_EXCERPT_CHARS: usize = 600;

/// The words a message may translate to. `answer` and `make-image` have external effects.
pub fn vocabulary() -> Vec<Ops> {
    vec![
        Ops::ReadFile,
        Ops::MakeImage,
        Ops::ListDirectory,
        Ops::ListFiles,
        Ops::Grep,
        Ops::Print,
        Ops::Answer,
        Ops::Fetch,
        Ops::Store,
        Ops::ShowRegisters,
        Ops::Models,
        Ops::Help,
        Ops::Add,
        Ops::Subtract,
        Ops::Multiply,
        Ops::Divide,
        Ops::Dup,
        Ops::Swap,
        Ops::Drop,
    ]
}

pub fn vocabulary_with(user_words: &[UserWord]) -> Vec<Ops> {
    let mut vocabulary = vocabulary();
    vocabulary.extend(user_words.iter().map(|word| Ops::UserCall {
        name: word.name.clone(), inputs: word.inputs.iter().map(|p| p.kind).collect(), outputs: word.outputs.iter().map(|p| p.kind).collect(),
    }));
    vocabulary
}

/// Deepest stack a translated program may build; real requests stay well within it.
const MAX_STACK_DEPTH: usize = 3;

/// A stack slot's type as the grammar tracks it. Registers never sit on the stack in
/// translated programs: the grammar only allows them directly before `@` or `!`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Slot {
    Text,
    Int,
    List,
    Address,
    Any,
}

impl Slot {
    fn of(kind: TypeKind) -> Slot {
        match kind {
            TypeKind::String => Slot::Text,
            TypeKind::Int => Slot::Int,
            TypeKind::List => Slot::List,
            TypeKind::Address => Slot::Address,
            TypeKind::Bool | TypeKind::Register | TypeKind::Reference | TypeKind::Any => Slot::Any,
        }
    }

    fn kind(self) -> TypeKind {
        match self {
            Slot::Text => TypeKind::String,
            Slot::Int => TypeKind::Int,
            Slot::List => TypeKind::List,
            Slot::Address => TypeKind::Address,
            Slot::Any => TypeKind::Any,
        }
    }

    fn letter(self) -> char {
        match self {
            Slot::Text => 's',
            Slot::Int => 'i',
            Slot::List => 'l',
            Slot::Address => 'd',
            Slot::Any => 'a',
        }
    }
}

/// The stack after `op` runs on `stack`, or `None` if its inputs are not there or the
/// result would be too deep.
fn apply(op: &Ops, stack: &[Slot]) -> Option<Vec<Slot>> {
    let mut stack = stack.to_vec();
    match op {
        Ops::Dup => stack.push(*stack.last()?),
        Ops::Swap => {
            let n = stack.len();
            if n < 2 {
                return None;
            }
            stack.swap(n - 1, n - 2);
        }
        _ => {
            for input in op.inputs().iter().rev() {
                if !input.kind.accepts(stack.pop()?.kind()) {
                    return None;
                }
            }
            stack.extend(op.outputs().iter().map(|t| Slot::of(t.kind)));
        }
    }
    (stack.len() <= MAX_STACK_DEPTH).then_some(stack)
}

fn state(prefix: char, stack: &[Slot]) -> String {
    let letters: String = stack.iter().map(|slot| slot.letter()).collect();
    format!("{prefix}{letters}")
}

/// The next stacks reachable from `stack`, each with the text that gets there.
fn transitions(stack: &[Slot], in_block: bool, vocabulary: &[Ops]) -> Vec<(String, Vec<Slot>)> {
    let mut next = vec![];
    let push = |slot: Slot| {
        let mut grown = stack.to_vec();
        grown.push(slot);
        (grown.len() <= MAX_STACK_DEPTH).then_some(grown)
    };
    if let Some(grown) = push(Slot::Text) {
        next.push(("string".to_string(), grown));
    }
    if let Some(grown) = push(Slot::Int) {
        next.push(("integer".to_string(), grown));
    }
    if let Some(grown) = push(Slot::Any) {
        next.push((r#"register " @""#.to_string(), grown));
    }
    if let Some((top, rest)) = stack.split_last() {
        next.push((r#"register " !""#.to_string(), rest.to_vec()));
        // Blocks do not nest, which keeps the grammar a fixed size.
        if !in_block && matches!(top, Slot::List | Slot::Any) {
            next.push((r#"block " each""#.to_string(), rest.to_vec()));
        }
        if *top == Slot::Address {
            let mut fetched = rest.to_vec();
            fetched.push(Slot::Any);
            next.push((format!("{:?}", "@"), fetched));
            if let Some((_, below)) = rest.split_last() { next.push((format!("{:?}", "!"), below.to_vec())); }
        }
    }
    for op in vocabulary.iter().filter(|op| !matches!(op, Ops::Fetch | Ops::Store)) {
        if let Some(after) = apply(op, stack) {
            next.push((format!("{:?}", op.token_name()), after));
        }
    }
    next
}

/// A GBNF grammar whose every program type-checks. Each nonterminal stands for a stack
/// shape (`x` before an item, `t` after one), so a word is only offered when the stack
/// holds its inputs: the model cannot write `list_directory` before a path.
pub fn grammar() -> String {
    grammar_with(&[])
}

pub fn grammar_with(user_words: &[UserWord]) -> String {
    let vocabulary = vocabulary_with(user_words);
    let mut rules = vec![];
    add_states(&mut rules, vec![], false, &vocabulary);
    add_states(&mut rules, vec![Slot::Any], true, &vocabulary);
    format!(
        r#"root ::= x
block ::= "[ " ya
{}
string ::= "\"" char* "\""
char ::= [^"\\\x00-\x1f] | "\\" ["\\nrt]
integer ::= "-"? [0-9] [0-9]? [0-9]? [0-9]? [0-9]? [0-9]?
register ::= "R" ("1" [0-5] | [0-9])
"#,
        rules.join("\n")
    )
}

/// Adds a rule pair for every stack reachable from `start`. At top level, `x` precedes an
/// item and `t` follows one, ending anywhere. In a block, `y` and `u` play those roles, and
/// `]` may only follow once the item is used up, so blocks always leave the stack as found.
fn add_states(rules: &mut Vec<String>, start: Vec<Slot>, in_block: bool, vocabulary: &[Ops]) {
    let (before, after) = if in_block { ('y', 'u') } else { ('x', 't') };
    let mut seen = std::collections::BTreeSet::new();
    let mut pending = vec![start];
    while let Some(stack) = pending.pop() {
        if !seen.insert(stack.clone()) {
            continue;
        }
        let alternatives: Vec<String> = transitions(&stack, in_block, vocabulary)
            .into_iter()
            .map(|(text, next)| {
                let rule = format!("{text} {}", state(after, &next));
                pending.push(next);
                rule
            })
            .collect();
        rules.push(format!("{} ::= {}", state(before, &stack), alternatives.join(" | ")));
        let end = match (in_block, stack.is_empty()) {
            (false, _) => r#""" | "#,
            (true, true) => r#"" ]" | "#,
            (true, false) => "",
        };
        rules.push(format!(r#"{} ::= {end}" " {}"#, state(after, &stack), state(before, &stack)));
    }
}

/// Whether the grammar admits a compiled program, by walking the same stack states.
fn grammar_admits(steps: &[Ops]) -> bool {
    walk(steps, vec![], false).is_some()
}

/// The stack after walking `steps` through the grammar's states, or `None` if it rejects them.
fn walk(steps: &[Ops], mut stack: Vec<Slot>, in_block: bool) -> Option<Vec<Slot>> {
    let mut steps = steps.iter();
    while let Some(step) = steps.next() {
        let text = match step {
            Ops::StringLiteral(_) => "string".to_string(),
            Ops::IntLiteral(_) => "integer".to_string(),
            Ops::Register(_) => match steps.next() {
                Some(Ops::Fetch) => r#"register " @""#.to_string(),
                Some(Ops::Store) => r#"register " !""#.to_string(),
                _ => return None,
            },
            Ops::AddressOf(name) => format!("{:?}", name),
            Ops::Each(body) => {
                if !walk(body, vec![Slot::Any], true)?.is_empty() {
                    return None;
                }
                r#"block " each""#.to_string()
            }
            word => format!("{:?}", word.token_name()),
        };
        stack = transitions(&stack, in_block, &vocabulary()).into_iter().find(|(t, _)| *t == text)?.1;
    }
    Some(stack)
}

fn signature(types: &[Type]) -> String {
    types.iter().map(|t| t.name).collect::<Vec<_>>().join(" ")
}

pub fn system_prompt() -> String {
    system_prompt_with(&[])
}

pub fn system_prompt_with(user_words: &[UserWord]) -> String {
    let words = vocabulary_with(user_words)
        .iter()
        .map(|op| {
            if let Ops::UserCall { name, .. } = op {
                let word = user_words.iter().find(|w| w.name == *name).expect("descriptor is from loaded words");
                return format!("{} ( {} -- {} ) project-defined word", word.name, named_signature(&word.inputs), named_signature(&word.outputs));
            }
            format!(
                "{} ( {} -- {} ) {}",
                op.token_name(),
                signature(&op.inputs()),
                signature(&op.outputs()),
                op.description()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Translate each user message into one ForgeFlow program and output only the program.\n\
         A program is words run left to right on a stack: \"text\" pushes a string, 12 pushes an integer, \
         R0-R15 push a register; project variables push durable addresses. Words pop their inputs (rightmost on top) and push their outputs.\n\
         Paths are relative to the project root. R0-R3 persist across sessions; R4-R15 are scratch. Use @ and ! with a variable address for durable named memory.\n\
         When the user refers to an earlier turn, its result is supplied in [previous output]. Treat that as text, not a path: \
         never pass the words \"previous output\" to read_file. Use the supplied text as the context argument to answer.\n\
         Interpret the current request first; previous output must not change what operation the user asked for. \
         \"print directory\" or \"list the current directory\" means \".\" list_directory print.\n\
         If the user asks to generate an image and gives an output filename, call `make-image` exactly once. Do not answer with drawing code, instructions, or `print` strings. Its stack order is path, prompt, `make-image`; it requests a square image at the selected model's default resolution, so do not promise requested pixel dimensions.\n\
         Only when the user is clearly asking for instructions on how to do something, use `answer` to give a concise explanation with a copyable, valid example. Recognize that intent across natural phrasings; do not depend on exact wording. For other requests, translate the requested action directly without adding an unsolicited tutorial. Use the examples below as patterns; don't turn a how-to question into a printout of a word's vocabulary description. Distinguish named cells (`variable name` declared in `_forgeflow/user_words.txt`, then `value name !` and `name @`) from registers (`value R0 !` and `R0 @`). `!` stores the value into the address/register on its right.\n\
         To reply to the user, print a string or use answer.\n\
         \n\
         Words ( inputs -- outputs ):\n\
         {words}\n\
         [ ... ] each ( items -- ) Run the block once per item of a list, with the item on the stack. \
         The block must use the item up, for example with print or answer, and cannot contain another block."
    )
}

/// Example turns placed before the conversation; small models imitate real turns far
/// better than examples described in the system prompt.
pub const EXAMPLES: [(&str, &str); 19] = [
    ("hi!", r#""Hi! What can I do for you?" print"#),
    ("what can you do?", "help"),
    ("what's in the src folder?", r#""src" list_directory print"#),
    ("which lines of src/main.rs mention config?", r#""src/main.rs" read_file "config" grep print"#),
    ("summarize README.md", r#""README.md" read_file "Summarize this file." answer"#),
    ("keep notes.txt in R5", r#""notes.txt" read_file R5 !"#),
    (
        "[registers]\nR5 text 1.2k chars, 30 lines [\"notes.txt\" read_file R5 !] \"# Notes⏎…\"\n\nwhat's the title of that file?",
        r#"R5 @ "What is the title?" answer"#,
    ),
    ("remember that we deploy to staging", r#""We deploy to staging." R0 ! "Noted." print"#),
    ("explain what a monad is", r#""" "Explain what a monad is." answer"#),
    (
        "how do i store a value in a variable",
        r#""Named cells: add `variable score` to `_forgeflow/user_words.txt`, then store with `; 10 score !` and fetch with `; score @ print`. Registers need no declaration: use `; \"Sup\" R0 !` to store and `; R0 @ print` to fetch. The value comes before the variable or register, then `!`." "How do I store a value in a variable? Give a runnable example." answer"#,
    ),
    (
        "how do i get a value from a register",
        r#"Registers use `@` to fetch their value. For example, `; R0 @ print` fetches R0 and prints it; first store with `; \"Sup\" R0 !`. Registers R0-R3 persist across sessions." "How do I get a value from a register? Give a runnable example." answer"#,
    ),
    ("what is 12 times 7, minus 4?", "12 7 * 4 - print"),
    (
        "list the root folder, then show the models, then my registers",
        r#""." list_directory print models registers"#,
    ),
    ("show the models and read Cargo.toml", r#"models "Cargo.toml" read_file print"#),
    ("what files are in the notes folder?", r#""notes" list_files print"#),
    (
        "give a one-line summary of each file in the notes folder",
        r#""notes" list_files [ dup print read_file "Summarize this in one line." answer ] each"#,
    ),
    ("print directory", r#""." list_directory print"#),
    (
        "[previous output]\n.git/\nREADME.md\n\nprint directory",
        r#""." list_directory print"#,
    ),
    (
        "make an image of a banana and save to .tmp/ban.png",
        r#"".tmp/ban.png" "A banana" make-image"#,
    ),
];

/// What a message can ask for, generated from the vocabulary.
pub fn help_text() -> String {
    help_text_with(&[])
}

pub fn help_text_with(user_words: &[UserWord]) -> String {
    let words = vocabulary_with(user_words)
        .iter()
        .map(|op| {
            if let Ops::UserCall { name, .. } = op {
                let word = user_words.iter().find(|w| w.name == *name).expect("descriptor is from loaded words");
                return format!("  {:<14} ( {} -- {} ) project-defined word", word.name, named_signature(&word.inputs), named_signature(&word.outputs));
            }
            format!(
                "  {:<14} ( {} -- {} ) {}",
                op.token_name(),
                signature(&op.inputs()),
                signature(&op.outputs()),
                op.description()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let examples = EXAMPLES
        .iter()
        .filter(|(message, _)| !message.starts_with('['))
        .map(|(message, program)| format!("  {message:<45} → {program}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Ordinary messages go straight to the chat model without tools. Prefix a request with `;` to translate it into a ForgeFlow program and run it. In those requests, you can:\n\
         \x20 - reply, or answer questions about a file or in general\n\
         \x20 - list folders and read files in this project, or find lines that mention something\n\
         \x20 - do integer arithmetic, and several of these in one message\n\
         \x20 - keep values in registers: R0-R3 are remembered across sessions, R4-R15 last until you quit\n\
         \x20 - use named durable cells through their addresses and the Forth words @ and !\n\
         \x20 - define a persistent word with `; define a function called square that duplicates and multiplies its input`\n\
         \x20 - show registers or local models\n\
         \x20 - repeat steps for each item in a list, such as summarizing each file in a folder\n\
         \x20 - generate one image and save it inside this project\n\
         I can't write arbitrary files, run commands, or branch yet.\n\
         \n\
         For example:\n{examples}\n\
         \n\
         Words ( inputs -- outputs ):\n{words}\n\
         \x20 {:<14} ( items -- ) Run the block once per item of a list, with the item on the stack.\n\
         \n\
         Typing `; help` shows this without asking the model. Chat commands, also answered directly:\n\
         \x20 /model          show the current model and installed ones\n\
         \x20 /model <id>     switch models\n\
         \x20 /models         list models with fit and speed estimates\n\
         \x20 /reload         reload project-defined words\n\
         \x20 /clear          clear conversational history\n\
         \x20 /pull <id>      download a model",
        "[ ... ] each"
    )
}

fn named_signature(params: &[crate::user_words::Parameter]) -> String {
    params.iter().map(|p| format!("{} {:?}", p.name, p.kind)).collect::<Vec<_>>().join(" ")
}

/// Messages answered with `help` directly, skipping the model.
pub fn is_help_request(message: &str) -> bool {
    matches!(message.trim().to_lowercase().as_str(), "help" | "?" | "/help")
}

/// One completed turn: what the user said, the program it became, and what it showed.
#[derive(Debug, Clone, PartialEq)]
pub struct Exchange {
    pub message: String,
    pub program: String,
    pub output: String,
}

/// Chat messages for the translator. Past turns render identically every time, so the
/// server's prompt cache covers everything before the newest message.
pub fn messages(history: &[Exchange], message: &str, registers: &str) -> Vec<Json> {
    messages_with(history, message, registers, &[])
}

pub fn messages_with(history: &[Exchange], message: &str, registers: &str, user_words: &[UserWord]) -> Vec<Json> {
    let recent = &history[history_start(history.len())..];
    let mut messages = vec![json!({"role": "system", "content": system_prompt_with(user_words)})];
    for (user, program) in EXAMPLES {
        messages.push(json!({"role": "user", "content": user}));
        messages.push(json!({"role": "assistant", "content": program}));
    }
    let mut previous_output: Option<&str> = None;
    for exchange in recent {
        messages.push(json!({"role": "user", "content": user_content(previous_output, &exchange.message)}));
        messages.push(json!({"role": "assistant", "content": exchange.program}));
        previous_output = Some(&exchange.output);
    }
    let mut latest = user_content(previous_output, message);
    if !registers.is_empty() {
        latest = format!("[registers]\n{registers}\n\n{latest}");
    }
    messages.push(json!({"role": "user", "content": latest}));
    messages
}

/// Where the visible history begins. It moves a whole block at a time: sliding by one
/// turn would change the first history message every turn and void the prompt cache
/// from there on.
fn history_start(turns: usize) -> usize {
    (turns / HISTORY_BLOCK).saturating_sub(1) * HISTORY_BLOCK
}

fn user_content(previous_output: Option<&str>, message: &str) -> String {
    match previous_output {
        Some(output) if !output.trim().is_empty() => {
            let excerpt: String = output.chars().take(OUTPUT_EXCERPT_CHARS).collect();
            let cut = if output.chars().count() > OUTPUT_EXCERPT_CHARS { "…" } else { "" };
            format!("[previous output]\n{}{cut}\n\n{message}", excerpt.trim_end())
        }
        _ => message.to_string(),
    }
}

/// Parses, compiles, and type-checks a program, rejecting words outside the vocabulary
/// and fetches from registers that will be empty when the fetch runs.
pub fn compile(program: &str, filled: &dyn Fn(usize) -> bool) -> Result<Vec<Ops>, String> {
    compile_with(program, filled, &[])
}

pub fn compile_with(program: &str, filled: &dyn Fn(usize) -> bool, user_words: &[UserWord]) -> Result<Vec<Ops>, String> {
    let source = Source::new("<message>", program);
    let parsed = parser::parse(&source).map_err(|errors| describe(&errors))?;
    let descriptors: HashMap<String, Ops> = user_words.iter().map(|w| (w.name.clone(), Ops::UserCall { name: w.name.clone(), inputs: w.inputs.iter().map(|p| p.kind).collect(), outputs: w.outputs.iter().map(|p| p.kind).collect() })).collect();
    let steps = words::compile_with(&parsed.tokens, &descriptors, &HashMap::new()).map_err(|errors| describe(&errors))?;
    if let Some(step) = unavailable(&steps, &vocabulary_with(user_words)) {
        return Err(format!("`{}` is not available here", step.name()));
    }
    workflow::check_steps(vec![], &steps)?;
    let mut stored = [false; REGISTER_COUNT];
    for pair in steps.windows(2) {
        match pair {
            [Ops::Register(r), Ops::Store] => stored[*r] = true,
            [Ops::Register(r), Ops::Fetch] if !stored[*r] && !filled(*r) => {
                return Err(format!("R{r} is empty, so `R{r} @` has nothing to fetch"));
            }
            _ => {}
        }
    }
    Ok(steps)
}

/// The first word, inside blocks too, that the vocabulary does not include.
fn unavailable<'a>(steps: &'a [Ops], allowed: &[Ops]) -> Option<&'a Ops> {
    steps.iter().find_map(|step| match step {
        Ops::Each(body) => unavailable(body, allowed),
        step if is_word(step) && !allowed.contains(step) => Some(step),
        _ => None,
    })
}

fn is_word(step: &Ops) -> bool {
    !matches!(
        step,
        Ops::StringLiteral(_) | Ops::IntLiteral(_) | Ops::BoolLiteral(_) | Ops::Register(_)
    )
}

fn describe(errors: &[parser::ParseError]) -> String {
    errors.iter().map(|e| e.message.clone()).collect::<Vec<_>>().join("; ")
}

/// Sends a one-token request so the server caches the system prompt before the first message.
pub fn warm_up(backend: &dyn ChatBackend) -> Result<(), String> {
    let options = StepOptions {
        grammar: Some(grammar()),
        max_tokens: Some(1),
        no_thinking: true,
        slot: Some(TRANSLATE_SLOT),
        ..StepOptions::default()
    };
    backend.step(&messages(&[], "hi", ""), &[], &options, &mut |_| {}).map(|_| ())
}

/// A message's program, ready to run.
#[derive(Debug, PartialEq)]
pub struct Translation {
    pub program: String,
    pub steps: Vec<Ops>,
    /// Model calls used: 2 means the first program failed and was retried.
    pub attempts: usize,
}

/// Asks the model for a program, retrying once with the error if it does not compile.
pub fn translate(
    backend: &dyn ChatBackend,
    history: &[Exchange],
    message: &str,
    registers: &Registers,
) -> Result<Translation, String> {
    translate_with(backend, history, message, registers, &[])
}

pub fn translate_with(
    backend: &dyn ChatBackend,
    history: &[Exchange],
    message: &str,
    registers: &Registers,
    user_words: &[UserWord],
) -> Result<Translation, String> {
    let filled = |r: usize| registers.is_filled(r);
    let options = StepOptions {
        grammar: Some(grammar_with(user_words)),
        temperature: Some(0.0),
        max_tokens: Some(MAX_PROGRAM_TOKENS),
        no_thinking: true,
        slot: Some(TRANSLATE_SLOT),
    };
    let mut messages = messages_with(history, message, &registers.summary(), user_words);
    let mut last_error = String::new();
    for attempt in 1..=2 {
        let turn = backend.step(&messages, &[], &options, &mut |_| {})?;
        let program = turn.content.trim().to_string();
        match compile_with(&program, &filled, user_words) {
            Ok(steps) if image_file_request(message) && !valid_image_program(&steps) => {
                last_error = "this request asks to generate and save an image; use `make-image` exactly once and do not print instructions or answer text".into();
                messages.push(json!({"role": "assistant", "content": program}));
                messages.push(json!({"role": "user", "content": format!("That program is invalid: {last_error}. Return only a program shaped like `\"path\" \"image prompt\" make-image`; use the requested output path, and do not add print or answer.")}));
            }
            Ok(steps) if has_previous_output(history) && reads_previous_output_as_file(&steps) => {
                last_error = format!(
                    "`{program}`: `previous output` is conversation text, not a file path; use the supplied text as the context for `answer`"
                );
                messages.push(json!({"role": "assistant", "content": program}));
                messages.push(json!({"role": "user", "content": format!("That program is invalid: {last_error}. Output a corrected program.")}));
            }
            Ok(steps) => {
                return Ok(Translation {
                    program,
                    steps,
                    attempts: attempt,
                });
            }
            Err(error) => {
                last_error = format!("`{program}`: {error}");
                messages.push(json!({"role": "assistant", "content": program}));
                messages.push(json!({"role": "user", "content": format!("That program is invalid: {error}. Output a corrected program.")}));
            }
        }
    }
    Err(format!("could not translate the message into a valid program ({last_error})"))
}

fn image_file_request(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    let image_request = ["image", "picture", "illustration", "sprite", "icon"]
        .iter()
        .any(|word| message.contains(word));
    let output_file = [".png", ".webp", ".jpg", ".jpeg"]
        .iter()
        .any(|extension| message.contains(extension));
    let save_action = ["save", "write", "output", "store", "export"]
        .iter()
        .any(|verb| message.contains(verb));
    let generation_action = ["generate", "draw", "render"]
        .iter()
        .any(|verb| message.contains(verb))
        || (["make", "create"].iter().any(|verb| message.contains(verb))
            && !message.contains("make a list")
            && !message.contains("create a list"));
    image_request && output_file && save_action && generation_action
}

fn valid_image_program(steps: &[Ops]) -> bool {
    let mut count = 0;
    for step in steps {
        match step {
            Ops::MakeImage => count += 1,
            Ops::StringLiteral(_) => {}
            _ => return false,
        }
    }
    count == 1
}

fn has_previous_output(history: &[Exchange]) -> bool {
    history.last().is_some_and(|turn| !turn.output.trim().is_empty())
}

/// Rejects the model's common misreading of the `[previous output]` context label as a filename.
fn reads_previous_output_as_file(steps: &[Ops]) -> bool {
    steps.windows(2).any(|pair| {
        matches!(pair, [Ops::StringLiteral(path), Ops::ReadFile]
            if path.trim().eq_ignore_ascii_case("previous output"))
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::chat::{Delta, Turn};

    struct Scripted(RefCell<Vec<&'static str>>);

    impl ChatBackend for Scripted {
        fn step(&self, _: &[Json], _: &[Json], options: &StepOptions, _: &mut dyn FnMut(Delta)) -> Result<Turn, String> {
            assert!(options.grammar.is_some());
            Ok(Turn {
                content: self.0.borrow_mut().remove(0).to_string(),
                ..Turn::default()
            })
        }
    }

    fn compile(program: &str) -> Result<Vec<Ops>, String> {
        super::compile(program, &|r| r == 5)
    }

    #[test]
    fn every_example_compiles() {
        for (_, program) in EXAMPLES {
            assert!(compile(program).is_ok(), "{program}: {:?}", compile(program));
        }
    }

    #[test]
    fn rejects_fetches_from_empty_registers() {
        assert!(compile("R5 @ print").is_ok());
        assert_eq!(compile("R4 @ print").unwrap_err(), "R4 is empty, so `R4 @` has nothing to fetch");
        assert!(compile("\"x\" R4 ! R4 @ print").is_ok());
    }

    #[test]
    fn compile_rejects_bad_programs() {
        let cases = [
            ("\"x\" read_file", true),
            ("\"x\" write_file", false),
            ("\"x\" agent", false),
            ("read_file", false),
            ("3 print", true),
            ("R5 @ \"x\" grep", true),
            ("R5 @ 2 grep", false),
            ("5 \"a\" grep", false),
        ];
        for (program, ok) in cases {
            assert_eq!(compile(program).is_ok(), ok, "{program}");
        }
    }

    #[test]
    fn grammar_offers_every_vocabulary_word() {
        let grammar = grammar();
        for op in vocabulary() {
            let listed = grammar.contains(&format!("{:?}", op.name()));
            assert_eq!(listed, !matches!(op, Ops::Fetch | Ops::Store), "{}", op.name());
        }
    }

    #[test]
    fn custom_words_appear_in_translation_vocabulary_grammar_and_compile() {
        let word = UserWord {
            name: "increment".into(),
            inputs: vec![crate::user_words::Parameter { name: "n".into(), kind: TypeKind::Int }],
            outputs: vec![crate::user_words::Parameter { name: "result".into(), kind: TypeKind::Int }],
            body: vec![], source: "words.ff".into(),
        };
        assert!(grammar_with(std::slice::from_ref(&word)).contains(r#""increment""#));
        assert!(system_prompt_with(std::slice::from_ref(&word)).contains("increment ( n Int -- result Int )"));
        let steps = compile_with("5 increment", &|_| false, std::slice::from_ref(&word)).unwrap();
        assert!(matches!(steps[1], Ops::UserCall { ref name, .. } if name == "increment"));
    }

    #[test]
    fn address_words_can_be_fetched_or_stored_in_generated_grammar() {
        let address = UserWord { name: "score".into(), inputs: vec![], outputs: vec![crate::user_words::Parameter { name: "address".into(), kind: TypeKind::Address }], body: vec![], source: "memory.ff".into() };
        let grammar = grammar_with(std::slice::from_ref(&address));
        assert!(grammar.contains(r#""score""#));
        assert!(grammar.contains(r#""@""#));
        assert!(grammar.contains(r#""!""#));
        assert!(compile_with("score @ print", &|_| false, std::slice::from_ref(&address)).is_ok());
        assert!(compile_with("42 score !", &|_| false, std::slice::from_ref(&address)).is_ok());
    }

    #[test]
    fn grammar_admits_every_example() {
        for (_, program) in EXAMPLES {
            assert!(grammar_admits(&compile(program).unwrap()), "{program}");
        }
    }

    #[test]
    fn grammar_rejects_words_before_their_inputs() {
        let words_only = |program: &str| {
            let parsed = parser::parse(&Source::new("t", program)).unwrap();
            words::compile(&parsed.tokens).unwrap()
        };
        let cases = [
            (r#""." list_directory print models"#, true),
            (r#"list_directory "." print"#, false),
            (r#"5 read_file"#, false),
            (r#"R4 @ "x" grep print"#, true),
            (r#""x" R4"#, false),
            (r#"1 2 3 4"#, false),
        ];
        for (program, admitted) in cases {
            assert_eq!(grammar_admits(&words_only(program)), admitted, "{program}");
        }
    }

    #[test]
    fn grammar_admits_only_blocks_that_use_up_their_item() {
        let steps = |program: &str| {
            let parsed = parser::parse(&Source::new("t", program)).unwrap();
            words::compile(&parsed.tokens).unwrap()
        };
        let cases = [
            (r#""d" list_files [ dup print read_file "q" answer ] each"#, true),
            (r#""d" list_files [ print ] each registers"#, true),
            (r#""d" list_files [ read_file ] each"#, false),
            (r#""d" list_files [ "d" list_files [ print ] each ] each"#, false),
            (r#"5 [ print ] each"#, false),
        ];
        for (program, admitted) in cases {
            assert_eq!(grammar_admits(&steps(program)), admitted, "{program}");
        }
        assert!(grammar().contains(r#"u ::= " ]" | " " y"#));
    }

    #[test]
    fn admitted_programs_type_check() {
        // Every state the grammar can reach must be a stack the checker agrees with.
        let programs = [
            r#""a" dup swap drop print"#,
            r#"1 2 + 3 * print"#,
            r#"R5 @ R5 @ answer"#,
            r#""f" read_file "p" grep R6 ! registers"#,
        ];
        for program in programs {
            let steps = compile(program).unwrap();
            assert!(grammar_admits(&steps), "{program}");
            assert!(workflow::check_steps(vec![], &steps).is_ok(), "{program}");
        }
    }

    #[test]
    fn past_turns_render_the_same_way_every_time() {
        let history = vec![Exchange {
            message: "what's in src?".into(),
            program: "\"src\" list_directory print".into(),
            output: "main.rs\nlib.rs".into(),
        }];
        let first = messages(&history, "open the first one", "R4 scratch …");
        let mut longer = history.clone();
        longer.push(Exchange {
            message: "open the first one".into(),
            program: "\"src/main.rs\" read_file print".into(),
            output: "fn main() {}".into(),
        });
        let second = messages(&longer, "thanks", "");
        let start = 1 + 2 * EXAMPLES.len();
        assert_eq!(first[..start + 2], second[..start + 2]);
        assert_eq!(
            first[start + 2]["content"],
            "[registers]\nR4 scratch …\n\n[previous output]\nmain.rs\nlib.rs\n\nopen the first one"
        );
        assert_eq!(second[start + 2]["content"], "[previous output]\nmain.rs\nlib.rs\n\nopen the first one");
    }

    #[test]
    fn history_window_moves_in_blocks() {
        let cases = [(0, 0), (7, 0), (8, 0), (15, 0), (16, 8), (23, 8), (24, 16)];
        for (turns, start) in cases {
            assert_eq!(history_start(turns), start, "{turns} turns");
        }
        // Within a block, adding a turn only appends to the prompt.
        let turn = |i: usize| Exchange {
            message: format!("m{i}"),
            program: format!("{i} print"),
            output: i.to_string(),
        };
        let nine: Vec<_> = (0..9).map(turn).collect();
        let ten: Vec<_> = (0..10).map(turn).collect();
        let a = messages(&nine, "next", "");
        let b = messages(&ten, "next", "");
        assert_eq!(a[..a.len() - 1], b[..a.len() - 1]);
    }

    #[test]
    fn retries_once_with_the_error() {
        let backend = Scripted(RefCell::new(vec!["read_file", "\"a.txt\" read_file print"]));
        let registers = Registers::in_memory();
        let translation = translate(&backend, &[], "show a.txt", &registers).unwrap();
        assert_eq!(translation.program, "\"a.txt\" read_file print");
        assert_eq!(translation.steps.len(), 3);
        assert_eq!(translation.attempts, 2);

        let backend = Scripted(RefCell::new(vec!["print", "print"]));
        assert!(translate(&backend, &[], "x", &registers).unwrap_err().contains("could not translate"));
    }
}

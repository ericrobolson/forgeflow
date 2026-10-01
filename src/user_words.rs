use std::collections::{HashMap, HashSet};

use walkdir::WalkDir;

use crate::{
    ops::Ops,
    parser::{self, Source, Token, TokenKind},
    project::Project,
    type_::TypeKind,
    value::Value,
    workflow::{self, Context},
};

#[derive(Debug, Clone, PartialEq)]
pub struct Parameter { pub name: String, pub kind: TypeKind }

#[derive(Debug, Clone, PartialEq)]
pub struct UserWord {
    pub name: String,
    pub inputs: Vec<Parameter>,
    pub outputs: Vec<Parameter>,
    pub body: Vec<Ops>,
    pub source: String,
}

#[derive(Debug)]
struct RawWord { name: String, inputs: Vec<Parameter>, outputs: Vec<Parameter>, body: Vec<Token>, source: String, variable: bool }

/// Loads and checks every `.ff` below `_forgeflow`, shallow paths before deeper paths.
pub fn load(project: &Project) -> Result<Vec<UserWord>, String> {
    let folder = project.folder();
    let mut files = Vec::new();
    for entry in WalkDir::new(&folder) {
        let entry = entry.map_err(|e| format!("{}: {e}", e.path().unwrap_or(&folder).display()))?;
        if entry.file_type().is_file()
            && (entry.path().extension().is_some_and(|x| x == "ff")
                || entry.path() == folder.join("user_words.txt"))
        {
            files.push(entry.into_path());
        }
    }
    files.sort_by(|a,b| {
        let da = a.strip_prefix(&folder).map(|p| p.components().count()).unwrap_or(usize::MAX);
        let db = b.strip_prefix(&folder).map(|p| p.components().count()).unwrap_or(usize::MAX);
        da.cmp(&db).then_with(|| a.cmp(b))
    });
    let mut raw = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        let source = Source::new(file.display().to_string(), text);
        let tokens = parser::parse(&source).map_err(|errors| format_errors(&source, errors))?.tokens;
        raw.extend(parse_definitions(tokens, &source.name)?);
    }
    compile_definitions(raw, &[])
}

/// Replaces the active definitions only after the complete project set validates.
pub fn reload(context: &mut Context) -> Result<usize, String> {
    let words = load(&context.project)?;
    let count = words.len();
    context.user_words = words;
    Ok(count)
}

fn parse_definitions(tokens: Vec<Token>, source: &str) -> Result<Vec<RawWord>, String> {
    let mut words = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if matches!(tokens[i].kind, TokenKind::Comment(_)) { i += 1; continue; }
        if ident(&tokens[i], "variable") {
            let decl = &tokens[i];
            let name_token = tokens.get(i + 1).ok_or_else(|| at(source, decl, "`variable` needs a name"))?;
            let name = match &name_token.kind { TokenKind::Identifier(n) if valid_name(n) => n.clone(), _ => return Err(at(source, name_token, "expected a valid variable name")) };
            words.push(RawWord { name, inputs: vec![], outputs: vec![Parameter { name: "address".into(), kind: TypeKind::Address }], body: vec![], source: source.to_string(), variable: true });
            i += 2;
            if tokens.get(i).is_some_and(|t| ident(t, ";")) { i += 1; }
            continue;
        }
        if !ident(&tokens[i], ":") { return Err(at(source, &tokens[i], "expected `:` or `variable` to start a definition")); }
        i += 1;
        let name_token = tokens.get(i).ok_or_else(|| format!("{source}: expected a word name"))?;
        let name = match &name_token.kind { TokenKind::Identifier(n) if valid_name(n) => n.clone(), _ => return Err(at(source, name_token, "expected a valid word name")) };
        i += 1;
        if !tokens.get(i).is_some_and(|t| ident(t, "(")) { return Err(format!("{source}:{}: expected `(` after `{name}`", name_token.span.end)); }
        i += 1;
        let mut inputs = Vec::new();
        let mut outputs = Vec::new();
        let mut output_side = false;
        let mut params = HashSet::new();
        loop {
            let token = tokens.get(i).ok_or_else(|| format!("{source}: unterminated signature for `{name}`"))?;
            if ident(token, ")") { i += 1; break; }
            if ident(token, "--") {
                if output_side { return Err(at(source, token, "signature has more than one `--`")); }
                output_side = true; params.clear(); i += 1; continue;
            }
            let param_name = match &token.kind { TokenKind::Identifier(n) if valid_name(n) => n.clone(), _ => return Err(at(source, token, "expected a parameter name")) };
            if !params.insert(param_name.clone()) { return Err(at(source, token, format!("duplicate parameter `${param_name}`"))); }
            i += 1;
            let type_token = tokens.get(i).ok_or_else(|| format!("{source}: missing type for `${param_name}`"))?;
            let kind = parse_kind(type_token).ok_or_else(|| at(source, type_token, "expected String, Int, Bool, List, Register, or Any"))?;
            let parameter = Parameter { name: param_name, kind };
            if output_side { outputs.push(parameter) } else { inputs.push(parameter) }
            i += 1;
        }
        if !output_side { return Err(format!("{source}: signature for `{name}` needs `--`")); }
        let body_start = i;
        let mut depth = 0usize;
        while i < tokens.len() {
            if ident(&tokens[i], "[") { depth += 1; }
            if ident(&tokens[i], "]") { depth = depth.saturating_sub(1); }
            if depth == 0 && ident(&tokens[i], ";") { break; }
            i += 1;
        }
        if i == tokens.len() { return Err(format!("{source}: definition `{name}` is missing `;`")); }
        words.push(RawWord { name, inputs, outputs, body: tokens[body_start..i].to_vec(), source: source.to_string(), variable: false });
        i += 1;
    }
    Ok(words)
}

fn compile_definitions(raw: Vec<RawWord>, existing: &[UserWord]) -> Result<Vec<UserWord>, String> {
    let builtins: HashSet<String> = Ops::named_words().iter().map(|w| w.name().to_string()).collect();
    let mut names: HashMap<String, String> = existing.iter().map(|w| (w.name.clone(), w.source.clone())).collect();
    for word in &raw {
        if builtins.contains(&word.name) || ["true", "false", "each", "@", "!", "[", "]", "variable"].contains(&word.name.as_str()) || crate::registers::parse_register(&word.name).is_some() {
            return Err(format!("{}: `{}` conflicts with a built-in word or reserved name", word.source, word.name));
        }
        if let Some(previous) = names.insert(word.name.clone(), word.source.clone()) {
            return Err(format!("duplicate user word `{}` in {} and {}", word.name, previous, word.source));
        }
    }
    let mut descriptors: HashMap<String, Ops> = existing.iter().map(|w| (w.name.clone(), Ops::UserCall {
        name: w.name.clone(), inputs: w.inputs.iter().map(|p| p.kind).collect(), outputs: w.outputs.iter().map(|p| p.kind).collect(),
    })).collect();
    descriptors.extend(raw.iter().map(|w| (w.name.clone(), Ops::UserCall {
        name: w.name.clone(), inputs: w.inputs.iter().map(|p| p.kind).collect(), outputs: w.outputs.iter().map(|p| p.kind).collect(),
    })));
    let mut words = existing.to_vec();
    for word in raw {
        if word.variable {
            words.push(UserWord { name: word.name.clone(), inputs: vec![], outputs: word.outputs, body: vec![Ops::AddressOf(word.name)], source: word.source });
            continue;
        }
        let locals: HashMap<String, TypeKind> = word.inputs.iter().map(|p| (p.name.clone(), p.kind)).collect();
        let body = crate::words::compile_with(&word.body, &descriptors, &locals)
            .map_err(|errors| format!("{}: {}", word.source, errors.iter().map(|e| format!("{}..{}: {}", e.span.start, e.span.end, e.message)).collect::<Vec<_>>().join("; ")))?;
        let initial: Vec<_> = word.inputs.iter().map(|p| p.kind).collect();
        let expected: Vec<_> = word.outputs.iter().map(|p| p.kind).collect();
        let actual = workflow::check_steps(initial, &body).map_err(|e| format!("{}: word `{}`: {e}", word.source, word.name))?;
        if actual != expected { return Err(format!("{}: word `{}` declares outputs {:?}, but body leaves {:?}", word.source, word.name, expected, actual)); }
        words.push(UserWord { name: word.name, inputs: word.inputs, outputs: word.outputs, body, source: word.source });
    }
    validate_recursion(&words)?;
    Ok(words)
}

/// Compiles one generated definition against the currently active word set.
pub fn compile_one(existing: &[UserWord], source: &str, definition: &str) -> Result<Vec<UserWord>, String> {
    let parsed = parser::parse(&Source::new(source, definition)).map_err(|e| format!("{}", e.iter().map(|e| e.message.as_str()).collect::<Vec<_>>().join("; ")))?;
    let raw = parse_definitions(parsed.tokens, source)?;
    if raw.len() != 1 || raw[0].variable { return Err("expected exactly one `: name ( inputs -- outputs ) ... ;` word definition".into()); }
    compile_definitions(raw, existing)
}

/// Validates then durably appends a definition, returning the newly loaded word set.
pub fn add_definition(project: &Project, definition: &str) -> Result<Vec<UserWord>, String> {
    let existing = load(project)?;
    let folder = project.folder();
    let path = folder.join("user_words.txt");
    let label = path.display().to_string();
    compile_one(&existing, &label, definition)?;
    let prior = std::fs::read_to_string(&path).unwrap_or_default();
    let source = format!("{}{}{}", prior, if prior.is_empty() || prior.ends_with('\n') { "" } else { "\n" }, definition.trim());
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    write_atomic(&path, source.as_bytes())?;
    match load(project) {
        Ok(loaded) => Ok(loaded),
        Err(error) => {
            // Roll back if interactions with other source files invalidate the combined set.
            let rollback = if prior.is_empty() { std::fs::remove_file(&path).map_err(|e| e.to_string()) }
                else { write_atomic(&path, prior.as_bytes()) };
            if let Err(rollback_error) = rollback { return Err(format!("{error}; also failed to roll back saved definition: {rollback_error}")); }
            Err(error)
        }
    }
}

fn write_atomic(path: &std::path::Path, contents: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let folder = path.parent().ok_or_else(|| "definition path has no parent folder".to_string())?;
    let temporary = folder.join(format!(".user-words-{}.tmp", uuid::Uuid::now_v7()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temporary).map_err(|e| e.to_string())?;
        file.write_all(contents).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        std::fs::rename(&temporary, path).map_err(|e| e.to_string())?;
        Ok::<(), String>(())
    })();
    if result.is_err() { let _ = std::fs::remove_file(&temporary); }
    result
}

fn validate_recursion(words: &[UserWord]) -> Result<(), String> {
    let edges: HashMap<&str, Vec<&str>> = words.iter().map(|w| (w.name.as_str(), calls(&w.body).into_iter().collect())).collect();
    for word in words {
        let recursive: HashSet<&str> = edges[ word.name.as_str() ].iter().copied().filter(|target| reaches(*target, &word.name, &edges, &mut HashSet::new())).collect();
        if recursive.is_empty() { continue; }
        let Some(Ops::UserCall { name, .. }) = word.body.last() else {
            return Err(format!("{}: recursive calls in `{}` must be in tail position", word.source, word.name));
        };
        if !recursive.contains(name.as_str()) { return Err(format!("{}: recursive calls in `{}` must be the final word", word.source, word.name)); }
        if contains_call_in_each(&word.body) { return Err(format!("{}: recursive calls inside `each` are not supported", word.source)); }
    }
    Ok(())
}

fn calls(ops: &[Ops]) -> Vec<&str> {
    let mut out = Vec::new();
    for op in ops { match op { Ops::UserCall { name, .. } => out.push(name.as_str()), Ops::Each(body) => out.extend(calls(body)), _ => {} } }
    out
}
fn contains_call_in_each(ops: &[Ops]) -> bool { ops.iter().any(|op| match op { Ops::Each(body) => !calls(body).is_empty() || contains_call_in_each(body), _ => false }) }
fn reaches<'a>(from: &'a str, target: &str, edges: &HashMap<&'a str, Vec<&'a str>>, seen: &mut HashSet<&'a str>) -> bool {
    if from == target { return true; }
    if !seen.insert(from) { return false; }
    edges.get(from).is_some_and(|next| next.iter().any(|n| reaches(n, target, edges, seen)))
}

/// Executes a word. Final calls are trampolined, so tail recursion reuses one frame.
pub fn execute(context: &mut Context, name: &str) -> Result<(), String> {
    const MAX_NON_TAIL_DEPTH: usize = 256;
    if context.call_depth >= MAX_NON_TAIL_DEPTH { return Err(format!("user word call depth exceeded {MAX_NON_TAIL_DEPTH}")); }
    context.call_depth += 1;
    let result = execute_trampoline(context, name);
    context.call_depth -= 1;
    result
}

fn execute_trampoline(context: &mut Context, initial: &str) -> Result<(), String> {
    let mut current = initial.to_string();
    let make_frame = |context: &Context, word: &UserWord| -> Result<HashMap<String, Value>, String> {
        if context.stack.len() < word.inputs.len() { return Err(format!("`{}` needs {} input(s), got {}", word.name, word.inputs.len(), context.stack.len())); }
        let values = &context.stack[context.stack.len()-word.inputs.len()..];
        Ok(word.inputs.iter().zip(values).map(|(p,v)| (p.name.clone(), v.clone())).collect())
    };
    let frame = make_frame(context, context.user_words.iter().find(|w| w.name == current).ok_or_else(|| format!("unknown user word `{current}`"))?)?;
    context.local_frames.push(frame);
    let result = loop {
        let Some(word) = context.user_words.iter().find(|w| w.name == current).cloned() else { break Err(format!("unknown user word `{current}`")); };
        if let Some(Ops::UserCall { name: next, .. }) = word.body.last() {
            if word.body.len() > 1 { if let Err(e) = workflow::run_steps(context, &word.body[..word.body.len()-1]) { break Err(e); } }
            let next_word = context.user_words.iter().find(|w| w.name == *next).ok_or_else(|| format!("unknown user word `{next}`"));
            let next_word = match next_word { Ok(w) => w, Err(e) => break Err(e) };
            match make_frame(context, next_word) { Ok(frame) => *context.local_frames.last_mut().unwrap() = frame, Err(e) => break Err(e) }
            current = next.clone();
        } else {
            break workflow::run_steps(context, &word.body);
        }
    };
    context.local_frames.pop();
    result
}

fn valid_name(name: &str) -> bool { !name.is_empty() && !name.starts_with('$') && name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '?') }
fn ident(token: &Token, expected: &str) -> bool { matches!(&token.kind, TokenKind::Identifier(s) if s == expected) }
fn parse_kind(token: &Token) -> Option<TypeKind> { match &token.kind { TokenKind::Identifier(s) => match s.as_str() { "String" => Some(TypeKind::String), "Int" => Some(TypeKind::Int), "Bool" => Some(TypeKind::Bool), "List" => Some(TypeKind::List), "Register" => Some(TypeKind::Register), "Address" => Some(TypeKind::Address), "Any" => Some(TypeKind::Any), _ => None }, _ => None } }
fn at(source: &str, token: &Token, message: impl AsRef<str>) -> String { format!("{source}:{}..{}: {}", token.span.start, token.span.end, message.as_ref()) }
fn format_errors(source: &Source, errors: Vec<parser::ParseError>) -> String { errors.into_iter().map(|e| at(&source.name, &Token { kind: TokenKind::Comment(String::new()), source_name: source.name.clone().into(), span: e.span }, e.message)).collect::<Vec<_>>().join("\n") }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{agent::Provider, ops::Ops, project::Project, registers::Registers};

    fn source_words(src: &str) -> Result<Vec<UserWord>, String> {
        let source = Source::new("test.ff", src);
        let tokens = parser::parse(&source).map_err(|e| format_errors(&source, e))?.tokens;
        compile_definitions(parse_definitions(tokens, "test.ff")?, &[])
    }
    fn context(words: Vec<UserWord>) -> Context {
        Context { stack: vec![], provider: Provider::Codex, registers: Registers::in_memory(), memory: crate::registers::Memory::in_memory(), project: Project { root: std::env::temp_dir() }, server: None, printed: String::new(), store_label: "!".into(), user_words: words, local_frames: vec![], call_depth: 0 }
    }

    #[test] fn loads_forward_calls_and_named_input_copies() {
        let words = source_words(": twice ( n Int -- result Int ) $n $n + swap drop ; : four ( -- result Int ) 2 twice ;").unwrap();
        let mut c = context(words); Ops::IntLiteral(2).process(&mut c).unwrap(); Ops::UserCall { name: "twice".into(), inputs: vec![TypeKind::Int], outputs: vec![TypeKind::Int] }.process(&mut c).unwrap();
        assert_eq!(c.stack, vec![Value::Int(4)]);
    }

    #[test] fn accepts_recursive_calls_in_tail_position() {
        assert!(source_words(": loop ( n Int -- result Int ) loop ;").is_ok());
        assert!(source_words(": even ( n Int -- result Int ) odd ; : odd ( n Int -- result Int ) even ;").is_ok());
        assert!(source_words(": first ( -- ) next ; : next ( -- ) first 1 drop ;").unwrap_err().contains("tail position"));
    }

    #[test] fn rejects_unknown_local_duplicate_and_bad_effect() {
        assert!(source_words(": x ( -- result Int ) $missing ;").unwrap_err().contains("unknown named input"));
        assert!(source_words(": x ( a Int a Int -- ) ;").unwrap_err().contains("duplicate parameter"));
        assert!(source_words(": x ( -- result Int ) \"text\" ;").unwrap_err().contains("declares outputs"));
        assert!(source_words(": identity ( value Int -- value Int ) ;").is_ok());
        assert!(source_words(": duplicate ( -- value Int value Bool ) ;").unwrap_err().contains("duplicate parameter"));
    }

    #[test] fn rejects_duplicates_builtin_collisions_and_unknown_words() {
        assert!(source_words(": x ( -- ) ; : x ( -- ) ;").unwrap_err().contains("duplicate user word"));
        assert!(source_words(": print ( -- ) ;").unwrap_err().contains("conflicts"));
        assert!(source_words("variable R2").unwrap_err().contains("conflicts"));
        assert!(source_words(": x ( -- ) nope ;").unwrap_err().contains("unknown word"));
    }

    #[test] fn rejects_non_tail_recursion_and_accepts_tail_recursion() {
        assert!(source_words(": loop ( n Int -- result Int ) loop 1 + ;").unwrap_err().contains("tail position"));
        assert!(source_words(": loop ( n Int -- result Int ) loop ;").is_ok());
    }

    #[test] fn tail_call_chain_reuses_the_active_frame_and_returns_values() {
        let words = source_words(": inner ( n Int -- result Int ) $n 1 + swap drop ; : outer ( n Int -- result Int ) inner ;").unwrap();
        let mut c = context(words);
        c.stack.push(Value::Int(4));
        execute(&mut c, "outer").unwrap();
        assert_eq!(c.stack, vec![Value::Int(5)]);
        assert_eq!(c.call_depth, 0);
        assert!(c.local_frames.is_empty());
    }

    #[test] fn durable_variable_addresses_can_be_passed_to_user_words_and_fetched() {
        let words = source_words("variable score : increment ( cell Address -- ) $cell @ 1 + $cell ! drop ;").unwrap();
        let mut c = context(words);
        c.stack.push(Value::Int(40));
        Ops::UserCall { name: "score".into(), inputs: vec![], outputs: vec![TypeKind::Address] }.process(&mut c).unwrap();
        Ops::Store.process(&mut c).unwrap();
        Ops::UserCall { name: "score".into(), inputs: vec![], outputs: vec![TypeKind::Address] }.process(&mut c).unwrap();
        Ops::UserCall { name: "increment".into(), inputs: vec![TypeKind::Address], outputs: vec![] }.process(&mut c).unwrap();
        Ops::UserCall { name: "score".into(), inputs: vec![], outputs: vec![TypeKind::Address] }.process(&mut c).unwrap();
        Ops::Fetch.process(&mut c).unwrap();
        assert_eq!(c.stack, vec![Value::Int(41)]);
    }

    #[test] fn uninitialized_memory_and_invalid_reference_stack_types_fail_clearly() {
        let mut c = context(source_words("variable score").unwrap());
        Ops::UserCall { name: "score".into(), inputs: vec![], outputs: vec![TypeKind::Address] }.process(&mut c).unwrap();
        assert!(Ops::Fetch.process(&mut c).unwrap_err().contains("uninitialized"));
        assert!(workflow::check_steps(vec![TypeKind::String], &[Ops::Fetch]).unwrap_err().contains("register or address"));
        assert_eq!(workflow::check_steps(vec![TypeKind::Address], &[Ops::Fetch]), Ok(vec![TypeKind::Any]));
    }

    #[test] fn trampolines_deep_tail_call_chains() {
        let mut source = String::new();
        for i in 0..300 {
            if i == 299 { source.push_str(&format!(": w{i} ( -- result Int ) 7 ; ")); }
            else { source.push_str(&format!(": w{i} ( -- result Int ) w{} ; ", i + 1)); }
        }
        let words = source_words(&source).unwrap();
        let mut c = context(words);
        execute(&mut c, "w0").unwrap();
        assert_eq!(c.stack, vec![Value::Int(7)]);
        assert_eq!(c.call_depth, 0);
        assert!(c.local_frames.is_empty());
    }

    #[test] fn local_reference_outside_a_word_is_an_error() {
        let mut c = context(vec![]);
        assert!(Ops::LocalGet("x".into(), TypeKind::Int).process(&mut c).unwrap_err().contains("no active input"));
    }

    #[test] fn definition_delimiters_must_be_closed() {
        assert!(source_words(": x ( -- ) 1").unwrap_err().contains("missing `;`"));
        assert!(source_words(": x ( -- ) ;").is_ok());
        assert!(source_words("1").unwrap_err().contains("expected `:`"));
    }

    #[test] fn variable_declarations_may_be_nested_and_share_the_word_namespace() {
        let words = source_words("variable score ; : balance ( -- value Address ) score ;").unwrap();
        assert_eq!(words.iter().map(|w| w.name.as_str()).collect::<Vec<_>>(), ["score", "balance"]);
        assert_eq!(words[0].outputs[0].kind, TypeKind::Address);
        assert!(source_words("variable x : x ( -- ) ;").unwrap_err().contains("duplicate user word"));
    }

    #[test] fn address_cells_can_be_passed_through_durable_registers() {
        let words = source_words("variable score").unwrap();
        let mut c = context(words);
        Ops::UserCall { name: "score".into(), inputs: vec![], outputs: vec![TypeKind::Address] }.process(&mut c).unwrap();
        Ops::Register(0).process(&mut c).unwrap(); Ops::Store.process(&mut c).unwrap();
        Ops::Register(0).process(&mut c).unwrap(); Ops::Fetch.process(&mut c).unwrap();
        assert_eq!(c.stack, vec![Value::Address("score".into())]);
    }

    #[test] fn loads_nested_ff_files_in_stable_order_and_ignores_other_files() {
        let root = std::env::temp_dir().join(format!("forgeflow-words-{}", std::process::id())); let _ = std::fs::remove_dir_all(&root);
        let project = Project::initialize(&root).unwrap();
        std::fs::write(project.folder().join("z.ff"), ": z ( -- ) ;").unwrap();
        std::fs::write(project.folder().join("user_words.txt"), ": custom ( -- ) ;").unwrap();
        std::fs::create_dir_all(project.folder().join("nested")).unwrap();
        std::fs::write(project.folder().join("nested/a.ff"), ": a ( -- ) ;").unwrap();
        std::fs::write(project.folder().join("ignored.txt"), "bad").unwrap();
        let words = load(&project).unwrap(); assert_eq!(words.iter().map(|w| w.name.as_str()).collect::<Vec<_>>(), ["custom", "z", "a"]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test] fn malformed_signatures_and_unclosed_blocks_report_errors() {
        for (source, expected) in [
            (": x -- ;", "expected `(`"),
            (": x ( -- ) 1", "missing `;`"),
            (": x ( a -- ) ;", "expected String"),
            (": x ( -- result Nope ) ;", "expected String"),
            (": x ( -- ) [ 1 ;", "missing `;`"),
            (": x ( -- ) $name ;", "unknown named input"),
        ] { let error = source_words(source).unwrap_err(); assert!(error.contains(expected), "{source}: {error}"); }
    }

    #[test] fn failed_reload_preserves_the_active_word_set() {
        let root = std::env::temp_dir().join(format!("forgeflow-reload-{}", std::process::id())); let _ = std::fs::remove_dir_all(&root);
        let project = Project::initialize(&root).unwrap();
        std::fs::write(project.folder().join("words.ff"), ": good ( -- ) ;").unwrap();
        let mut c = context(load(&project).unwrap());
        c.project = Project { root: root.clone() };
        std::fs::write(project.folder().join("words.ff"), ": broken ( -- ) unknown ;").unwrap();
        assert!(reload(&mut c).is_err());
        assert_eq!(c.user_words.iter().map(|w| w.name.as_str()).collect::<Vec<_>>(), ["good"]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test] fn add_definition_persists_and_returns_the_activated_word_set() {
        let root = std::env::temp_dir().join(format!("forgeflow-add-word-{}", uuid::Uuid::now_v7()));
        let project = Project::initialize(&root).unwrap();
        let words = add_definition(&project, ": square ( n Int -- result Int ) dup * ;").unwrap();
        assert!(words.iter().any(|w| w.name == "square"));
        assert!(load(&project).unwrap().iter().any(|w| w.name == "square"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test] fn invalid_or_duplicate_definition_does_not_change_persistent_file() {
        let root = std::env::temp_dir().join(format!("forgeflow-invalid-word-{}", uuid::Uuid::now_v7()));
        let project = Project::initialize(&root).unwrap();
        add_definition(&project, ": square ( n Int -- result Int ) dup * ;").unwrap();
        let path = project.folder().join("user_words.txt");
        let before = std::fs::read(&path).unwrap();
        assert!(add_definition(&project, ": square ( n Int -- result Int ) dup ;").is_err());
        assert!(add_definition(&project, ": broken ( n Int -- result Int ) unknown ;").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let _ = std::fs::remove_dir_all(root);
    }
}

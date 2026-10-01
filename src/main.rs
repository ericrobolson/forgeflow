use std::{error::Error, fs, io, path::Path};

use forgeflow::{
    agent::Provider,
    ops::Ops,
    parser::{self, ParseOutput, Source},
    project::Project,
    util, words,
    workflow::{self, Context},
};

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

fn run_file(path: &Path, mut context: Context) -> Result<(), Box<dyn Error>> {
    context.user_words = forgeflow::user_words::load(&context.project)?;
    let source = read_source(path)?;
    let parsed = parse_and_report(&source, false).map_err(|_| "source parse failed")?;
    let descriptors: std::collections::HashMap<String, Ops> = context.user_words.iter().map(|w| (w.name.clone(), Ops::UserCall {
        name: w.name.clone(), inputs: w.inputs.iter().map(|p| p.kind).collect(), outputs: w.outputs.iter().map(|p| p.kind).collect(),
    })).collect();
    let steps = words::compile_with(&parsed.tokens, &descriptors, &std::collections::HashMap::new()).map_err(|errors| {
        parser::print_errors(&source, &errors);
        "source compile failed"
    })?;
    workflow::check_steps(vec![], &steps)?;
    workflow::run_steps(&mut context, &steps)?;
    Ok(())
}

fn read_source(path: &Path) -> io::Result<Source> {
    Ok(Source::new(
        path.display().to_string(),
        fs::read_to_string(path)?,
    ))
}

fn main() -> Result<(), Box<dyn Error>> {
    let working_dir = std::env::current_dir()?;
    if !Project::exists() && !util::confirmation("Project not found. Create one?") {
        println!("Not creating project.");
        return Ok(());
    }
    let project = Project::initialize(&working_dir)?;
    let mut context = Context::new(project, Provider::Claude)?;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] => {
            context.user_words = forgeflow::user_words::load(&context.project)?;
            forgeflow::messages::run(context)
        }
        ["models"] => run_words(context, vec![Ops::Models]),
        ["pull", id] => run_words(context, vec![Ops::StringLiteral(id.to_string()), Ops::Pull]),
        ["use", id] => run_words(context, vec![Ops::StringLiteral(id.to_string()), Ops::UseModel]),
        [path] if !path.starts_with('-') && *path != "help" => run_file(Path::new(path), context),
        _ => {
            eprintln!("{USAGE}");
            Err("unrecognized arguments".into())
        }
    }
}

const USAGE: &str = "Usage:
  forgeflow              chat: each message becomes a program that runs deterministically
  forgeflow models       list local models with fit and speed estimates
  forgeflow pull <id>    download a model
  forgeflow use <id>     select the model
  forgeflow <file.ff>    run a ForgeFlow program";

fn run_words(mut context: Context, steps: Vec<Ops>) -> Result<(), Box<dyn Error>> {
    workflow::run_steps(&mut context, &steps)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_mode_formats_parsed_tokens_with_spans() {
        let source = Source::new("example.ff", "12 dup");
        let output = parse_and_report(&source, false).unwrap();
        assert_eq!(
            parser::format_token(&output.tokens[0]),
            "example.ff:0..2 Integer(12)"
        );
        assert_eq!(
            parser::format_token(&output.tokens[1]),
            "example.ff:3..6 Identifier(dup)"
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
    fn file_mode_loads_and_executes_project_words() {
        let root = std::env::temp_dir().join(format!("forgeflow-word-file-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let project = Project::initialize(&root).unwrap();
        fs::write(project.folder().join("words.ff"), ": double ( n Int -- result Int ) $n $n + swap drop ;").unwrap();
        let program = root.join("main.ff");
        fs::write(&program, "21 double print").unwrap();
        let context = Context::new(project, Provider::Codex).unwrap();
        run_file(&program, context).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}

use std::{error::Error, fs, io, path::Path};

use forgeflow::{
    parser::{self, ParseOutput, Source},
    project::Project,
    util,
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

fn main() -> Result<(), Box<dyn Error>> {
    let working_dir = std::env::current_dir()?;
    if !Project::exists() && !util::confirmation("Project not found. Create one?") {
        println!("Not creating project.");
        return Ok(());
    }
    let _project = Project::initialize(&working_dir);

    let mut args = std::env::args_os().skip(1);
    match (args.next(), args.next()) {
        (Some(path), None) => run_file(Path::new(&path)),
        (None, None) => forgeflow::repl::run(),
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
}

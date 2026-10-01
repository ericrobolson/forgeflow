use std::collections::HashMap;

use crate::{ops::Ops, parser::{ParseError, Token, TokenKind}, registers::parse_register, type_::TypeKind};

/// Turns parsed tokens into executable words, reporting unknown words with their spans.
/// `[ ... ] each` compiles to one `Each` step holding the block's words.
pub fn compile(tokens: &[Token]) -> Result<Vec<Ops>, Vec<ParseError>> {
    compile_with(tokens, &HashMap::new(), &HashMap::new())
}

pub fn compile_with(tokens: &[Token], user_words: &HashMap<String, Ops>, locals: &HashMap<String, TypeKind>) -> Result<Vec<Ops>, Vec<ParseError>> {
    let mut errors = vec![];
    let mut position = 0;
    let steps = compile_block(tokens, &mut position, None, &mut errors, user_words, locals);
    if errors.is_empty() { Ok(steps) } else { Err(errors) }
}

/// Compiles until the `]` closing the block opened by `open`, or to the end at top level.
fn compile_block(
    tokens: &[Token],
    position: &mut usize,
    open: Option<&Token>,
    errors: &mut Vec<ParseError>,
    user_words: &HashMap<String, Ops>,
    locals: &HashMap<String, TypeKind>,
) -> Vec<Ops> {
    let words = Ops::named_words();
    let mut steps = vec![];
    while let Some(token) = tokens.get(*position) {
        *position += 1;
        let step = match &token.kind {
            TokenKind::Comment(_) => continue,
            TokenKind::Integer(i) => Ops::IntLiteral(*i),
            TokenKind::String(s) => Ops::StringLiteral(s.clone()),
            TokenKind::Float(_) => {
                error(errors, token, "floats are not supported yet".into());
                continue;
            }
            TokenKind::Identifier(name) => match name.as_str() {
                "true" => Ops::BoolLiteral(true),
                "false" => Ops::BoolLiteral(false),
                "[" => {
                    let body = compile_block(tokens, position, Some(token), errors, user_words, locals);
                    match tokens.get(*position).map(|t| &t.kind) {
                        Some(TokenKind::Identifier(next)) if next == "each" => {
                            *position += 1;
                            Ops::Each(body)
                        }
                        _ => {
                            error(errors, token, "a [ ] block must be followed by `each`".into());
                            continue;
                        }
                    }
                }
                "]" if open.is_some() => return steps,
                "]" => {
                    error(errors, token, "`]` has no matching `[`".into());
                    continue;
                }
                "each" => {
                    error(errors, token, "`each` needs a [ ] block right before it".into());
                    continue;
                }
                _ if name.starts_with('$') => match locals.get(&name[1..]) {
                    Some(kind) => Ops::LocalGet(name[1..].to_string(), *kind),
                    None => {
                        error(errors, token, format!("unknown named input `{name}`"));
                        continue;
                    }
                },
                _ => match parse_register(name).filter(|_| !name.starts_with('$')) {
                    Some(register) => Ops::Register(register),
                    None => match user_words.get(name).or_else(|| words.iter().find(|w| w.name() == name)) {
                        Some(word) => word.clone(),
                        None => {
                            error(errors, token, format!("unknown word `{name}`"));
                            continue;
                        }
                    },
                },
            },
        };
        steps.push(step);
    }
    if let Some(open) = open {
        error(errors, open, "this `[` is never closed with `]`".into());
    }
    steps
}

fn error(errors: &mut Vec<ParseError>, token: &Token, message: String) {
    errors.push(ParseError {
        span: token.span.clone(),
        message,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{Source, parse};

    fn compile_text(text: &str) -> Result<Vec<Ops>, Vec<ParseError>> {
        compile(&parse(&Source::new("test.ff", text)).unwrap().tokens)
    }

    #[test]
    fn compiles_literals_registers_and_words() {
        assert_eq!(
            compile_text("\"src\" list_directory R4 ! R4 @ print 7 true # note").unwrap(),
            vec![
                Ops::StringLiteral("src".into()),
                Ops::ListDirectory,
                Ops::Register(4),
                Ops::Store,
                Ops::Register(4),
                Ops::Fetch,
                Ops::Print,
                Ops::IntLiteral(7),
                Ops::BoolLiteral(true),
            ]
        );
    }

    #[test]
    fn compiles_each_blocks() {
        assert_eq!(
            compile_text("\"src\" list_files [ dup print read_file print ] each").unwrap(),
            vec![
                Ops::StringLiteral("src".into()),
                Ops::ListFiles,
                Ops::Each(vec![Ops::Dup, Ops::Print, Ops::ReadFile, Ops::Print]),
            ]
        );
        let messages = |text: &str| -> Vec<String> {
            compile_text(text).unwrap_err().into_iter().map(|e| e.message).collect()
        };
        assert_eq!(messages("[ print ]"), ["a [ ] block must be followed by `each`"]);
        assert_eq!(messages("[ print"), ["this `[` is never closed with `]`", "a [ ] block must be followed by `each`"]);
        assert_eq!(messages("print ] each"), ["`]` has no matching `[`", "`each` needs a [ ] block right before it"]);
        assert_eq!(messages("[ frob ] each"), ["unknown word `frob`"]);
    }

    #[test]
    fn reports_unknown_words_and_floats_with_spans() {
        let errors = compile_text("dup frobnicate 1.5 $R4").unwrap_err();
        let messages: Vec<_> = errors.iter().map(|e| (e.span.clone(), e.message.as_str())).collect();
        assert_eq!(
            messages,
            vec![
                (4..14, "unknown word `frobnicate`"),
                (15..18, "floats are not supported yet"),
                (19..22, "unknown named input `$R4`"),
            ]
        );
    }
}

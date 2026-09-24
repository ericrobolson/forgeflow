use std::ops::Range;

use chumsky::{prelude::*, text};

/// The origin and contents of a piece of ForgeFlow source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub name: String,
    pub text: String,
}

impl Source {
    pub fn new(name: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            text: text.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    Integer(i64),
    Float(f64),
    String(String),
    Comment(String),
    Identifier(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    /// Name of the source file or REPL submission this token came from.
    pub source_name: std::sync::Arc<str>,
    /// UTF-8 byte offsets into `Source::text`, covering the original token.
    pub span: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub span: Range<usize>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParseOutput {
    pub tokens: Vec<Token>,
}

fn classify_atom<'src>(
    atom: String,
    span: SimpleSpan<usize>,
) -> Result<TokenKind, Rich<'src, char, SimpleSpan<usize>>> {
    if let Ok(value) = atom.parse::<i64>() {
        return Ok(TokenKind::Integer(value));
    }
    let has_digit = atom.chars().any(|ch| ch.is_ascii_digit());
    if has_digit && (atom.contains('.') || atom.contains('e') || atom.contains('E')) {
        let normalized = normalize_float(&atom);
        return normalized
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .map(TokenKind::Float)
            .ok_or_else(|| Rich::custom(span, format!("invalid floating-point literal `{atom}`")));
    }

    // Numeric-looking literals that overflow their representation are errors,
    // rather than identifiers that happen to contain only numeric characters.
    let unsigned = atom.strip_prefix(['+', '-']).unwrap_or(&atom);
    if !unsigned.is_empty()
        && unsigned
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == 'e' || c == 'E' || c == '+' || c == '-')
        && unsigned.chars().any(|c| c.is_ascii_digit())
    {
        return Err(Rich::custom(
            span,
            format!("invalid numeric literal `{atom}`"),
        ));
    }
    Ok(TokenKind::Identifier(atom))
}

fn normalize_float(atom: &str) -> String {
    let exponent_at = atom.find(['e', 'E']).unwrap_or(atom.len());
    let (mantissa, exponent) = atom.split_at(exponent_at);
    let mut normalized = mantissa.to_owned();
    if normalized.starts_with('.') {
        normalized.insert(0, '0');
    } else if normalized.starts_with("+.") || normalized.starts_with("-.") {
        normalized.insert(1, '0');
    }
    if normalized.ends_with('.') {
        normalized.push('0');
    }
    normalized.push_str(exponent);
    normalized
}

fn lexer<'src>() -> impl Parser<
    'src,
    &'src str,
    Vec<(TokenKind, SimpleSpan<usize>)>,
    extra::Err<Rich<'src, char, SimpleSpan<usize>>>,
> {
    let escaped = just('\\').ignore_then(choice((
        just('"').to('"'),
        just('\\').to('\\'),
        just('n').to('\n'),
        just('r').to('\r'),
        just('t').to('\t'),
    )));
    let string = just('"')
        .ignore_then(
            choice((escaped.boxed(), none_of("\\\"\r\n").boxed()))
                .repeated()
                .collect::<String>(),
        )
        .then_ignore(just('"'))
        .map(TokenKind::String)
        .map_with(|token, e| (token, e.span()));

    let comment = just('#')
        .ignore_then(none_of("\r\n").repeated().collect::<String>())
        .map(TokenKind::Comment)
        .map_with(|token, e| (token, e.span()));

    let atom = any()
        .filter(|c: &char| !c.is_whitespace() && *c != '#' && *c != '"')
        .repeated()
        .at_least(1)
        .to_slice()
        .try_map(|atom: &str, span| classify_atom(atom.to_owned(), span))
        .map_with(|token, e| (token, e.span()));

    let whitespace = text::whitespace()
        .at_least(1)
        .to(None::<(TokenKind, SimpleSpan<usize>)>);
    choice((whitespace, choice((string, comment, atom)).map(Some)))
        .repeated()
        .collect::<Vec<_>>()
        .map(|tokens| tokens.into_iter().flatten().collect())
        .then_ignore(end())
}

pub fn parse(source: &Source) -> Result<ParseOutput, Vec<ParseError>> {
    match lexer().parse(source.text.as_str()).into_result() {
        Ok(parsed) => Ok(ParseOutput {
            // Clone one shared source name for all tokens instead of allocating
            // a separate filename string for every token.
            tokens: {
                let source_name: std::sync::Arc<str> = source.name.clone().into();
                parsed
                    .into_iter()
                    .map(|(kind, span)| Token {
                        kind,
                        source_name: source_name.clone(),
                        span: span.start..span.end,
                    })
                    .collect()
            },
        }),
        Err(errors) => Err(errors
            .into_iter()
            .map(|error| ParseError {
                span: error.span().start..error.span().end,
                message: error.to_string(),
            })
            .collect()),
    }
}

pub fn format_token(token: &Token) -> String {
    let kind = match &token.kind {
        TokenKind::Integer(value) => format!("Integer({value})"),
        TokenKind::Float(value) => format!("Float({value})"),
        TokenKind::String(value) => format!("String({value:?})"),
        TokenKind::Comment(value) => format!("Comment({value:?})"),
        TokenKind::Identifier(value) => format!("Identifier({value})"),
    };
    format!(
        "{}:{}..{} {kind}",
        token.source_name, token.span.start, token.span.end
    )
}

pub fn print_errors(source: &Source, errors: &[ParseError]) {
    use ariadne::{Label, Report, ReportKind, Source as AriadneSource};
    for error in errors {
        let span = error.span.clone();
        let report = Report::build(ReportKind::Error, (source.name.as_str(), span.clone()))
            .with_message("Could not parse source")
            .with_label(
                Label::new((source.name.as_str(), span)).with_message(error.message.clone()),
            )
            .finish();
        let _ = report.eprint((
            source.name.as_str(),
            AriadneSource::from(source.text.as_str()),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_each_token_kind_and_tracks_source_spans() {
        let src = Source::new(
            "test.ff",
            "42 -7 .5 1. 2e-3 \"hello\\nworld\" dup # comment",
        );
        let parsed = parse(&src).unwrap();
        assert!(matches!(parsed.tokens[0].kind, TokenKind::Integer(42)));
        assert!(matches!(parsed.tokens[1].kind, TokenKind::Integer(-7)));
        assert!(matches!(parsed.tokens[2].kind, TokenKind::Float(v) if v == 0.5));
        assert!(matches!(parsed.tokens[3].kind, TokenKind::Float(v) if v == 1.0));
        assert!(
            matches!(parsed.tokens[4].kind, TokenKind::Float(v) if (v - 0.002).abs() < f64::EPSILON)
        );
        assert_eq!(
            parsed.tokens[5].kind,
            TokenKind::String("hello\nworld".into())
        );
        assert_eq!(parsed.tokens[6].kind, TokenKind::Identifier("dup".into()));
        assert_eq!(parsed.tokens[7].kind, TokenKind::Comment(" comment".into()));
        for token in parsed.tokens {
            assert_eq!(token.source_name.as_ref(), "test.ff");
            assert!(token.span.start < token.span.end);
            assert!(src.text.is_char_boundary(token.span.start));
            assert!(src.text.is_char_boundary(token.span.end));
        }
    }

    #[test]
    fn reports_unterminated_strings_and_numeric_overflow() {
        let unterminated = Source::new("bad.ff", "\"oops");
        assert!(parse(&unterminated).is_err());
        let unknown_escape = Source::new("bad.ff", "\"bad\\q\"");
        assert!(parse(&unknown_escape).is_err());
        let overflow = Source::new("bad.ff", "999999999999999999999999");
        assert!(parse(&overflow).is_err());
        let float_overflow = Source::new("bad.ff", "1e9999");
        assert!(parse(&float_overflow).is_err());
    }

    #[test]
    fn accepts_empty_source_and_comment_only_source() {
        assert!(
            parse(&Source::new("empty.ff", "  \n\t"))
                .unwrap()
                .tokens
                .is_empty()
        );
        assert_eq!(
            parse(&Source::new("comment.ff", "# only"))
                .unwrap()
                .tokens
                .len(),
            1
        );
    }

    #[test]
    fn comments_stop_at_lf_crlf_and_cr() {
        let src = Source::new(
            "comments.ff",
            "# first\nsecond # middle\r\nthird # final\rfourth",
        );
        let tokens = parse(&src).unwrap().tokens;
        let values: Vec<_> = tokens
            .iter()
            .map(|token| match &token.kind {
                TokenKind::Comment(comment) => format!("#{comment}"),
                TokenKind::Identifier(identifier) => identifier.clone(),
                other => panic!("unexpected token: {other:?}"),
            })
            .collect();
        assert_eq!(
            values,
            [
                "# first", "second", "# middle", "third", "# final", "fourth"
            ]
        );
    }

    #[test]
    fn comments_and_strings_end_at_their_own_delimiters() {
        let src = Source::new("boundaries.ff", "\"# not a comment\"# real comment\nnext");
        let tokens = parse(&src).unwrap().tokens;
        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[0].kind, TokenKind::String("# not a comment".into()));
        assert_eq!(tokens[1].kind, TokenKind::Comment(" real comment".into()));
        assert_eq!(tokens[2].kind, TokenKind::Identifier("next".into()));
    }

    #[test]
    fn accepts_signed_integer_limits_and_decimal_exponent_forms() {
        let src = Source::new(
            "numbers.ff",
            "9223372036854775807 -9223372036854775808 +12 .5 -.5 1. 1.e2 2e-3 -2.5E+3",
        );
        let tokens = parse(&src).unwrap().tokens;
        assert_eq!(tokens[0].kind, TokenKind::Integer(i64::MAX));
        assert_eq!(tokens[1].kind, TokenKind::Integer(i64::MIN));
        assert_eq!(tokens[2].kind, TokenKind::Integer(12));
        let floats: Vec<f64> = tokens[3..]
            .iter()
            .map(|token| match token.kind {
                TokenKind::Float(value) => value,
                ref other => panic!("expected float, got {other:?}"),
            })
            .collect();
        let expected = [0.5, -0.5, 1.0, 100.0, 0.002, -2500.0];
        for (actual, expected) in floats.iter().zip(expected) {
            assert!((actual - expected).abs() <= f64::EPSILON * expected.abs().max(1.0));
        }
    }

    #[test]
    fn malformed_numeric_looking_atoms_are_errors_but_forth_symbols_are_identifiers() {
        for malformed in ["9223372036854775808", "1e", "1e+", "1..2", "--2"] {
            assert!(
                parse(&Source::new("bad-number.ff", malformed)).is_err(),
                "expected {malformed:?} to fail"
            );
        }
        let tokens = parse(&Source::new("words.ff", ". + - * / dup? word-name"))
            .unwrap()
            .tokens;
        let words: Vec<_> = tokens
            .into_iter()
            .map(|token| match token.kind {
                TokenKind::Identifier(word) => word,
                other => panic!("expected identifier, got {other:?}"),
            })
            .collect();
        assert_eq!(words, [".", "+", "-", "*", "/", "dup?", "word-name"]);
    }

    #[test]
    fn decodes_supported_escapes_and_rejects_incomplete_or_raw_newline_strings() {
        let source = Source::new(
            "strings.ff",
            "\"quote: \\\" slash: \\\\ line:\\n tab:\\t return:\\r\"",
        );
        let tokens = parse(&source).unwrap().tokens;
        assert_eq!(
            tokens[0].kind,
            TokenKind::String("quote: \" slash: \\ line:\n tab:\t return:\r".into())
        );
        for invalid in ["\"dangling\\", "\"raw\nnewline\"", "\"unknown\\x\""] {
            assert!(
                parse(&Source::new("bad-string.ff", invalid)).is_err(),
                "expected {invalid:?} to fail"
            );
        }
        assert_eq!(
            parse(&Source::new("empty-string.ff", "\"\""))
                .unwrap()
                .tokens[0]
                .kind,
            TokenKind::String(String::new())
        );
    }

    #[test]
    fn spans_are_utf8_byte_ranges_over_original_tokens() {
        let src = Source::new("unicode.ff", "λ # 雪\n\"é\"");
        let tokens = parse(&src).unwrap().tokens;
        assert_eq!(&src.text[tokens[0].span.clone()], "λ");
        assert_eq!(&src.text[tokens[1].span.clone()], "# 雪");
        assert_eq!(&src.text[tokens[2].span.clone()], "\"é\"");
        assert_eq!(tokens[0].span, 0..2);
        assert_eq!(tokens[1].span, 3..8);
        assert_eq!(tokens[2].span, 9..13);
    }

    #[test]
    fn lexer_accepts_adjacent_tokens_and_all_common_whitespace() {
        let src = Source::new("spacing.ff", "1\t\n\r\u{000B}\u{000C}2\"s\"word");
        let tokens = parse(&src).unwrap().tokens;
        assert_eq!(tokens.len(), 4);
        assert_eq!(tokens[0].kind, TokenKind::Integer(1));
        assert_eq!(tokens[1].kind, TokenKind::Integer(2));
        assert_eq!(tokens[2].kind, TokenKind::String("s".into()));
        assert_eq!(tokens[3].kind, TokenKind::Identifier("word".into()));
    }

    #[test]
    fn parse_errors_have_in_bounds_spans_and_useful_messages() {
        let src = Source::new("error.ff", "prefix \"unterminated");
        let errors = parse(&src).unwrap_err();
        assert!(!errors.is_empty());
        for error in errors {
            assert!(error.span.start <= error.span.end);
            assert!(error.span.end <= src.text.len());
            assert!(!error.message.is_empty());
        }
    }
}

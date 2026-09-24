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
    if atom.contains('.') || atom.contains('e') || atom.contains('E') {
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
            tokens: parsed
                .into_iter()
                .map(|(kind, span)| Token {
                    kind,
                    span: span.start..span.end,
                })
                .collect(),
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
    format!("{}..{} {kind}", token.span.start, token.span.end)
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
}

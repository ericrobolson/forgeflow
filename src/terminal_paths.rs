//! Recognize terminal file drops without evaluating shell syntax or changing prose.
use std::path::{Path, PathBuf};

fn unescape_uri(value: &str) -> Option<String> {
    let uri = value.strip_prefix("file://")?;
    let path = if uri.starts_with('/') { uri }
        else { uri.strip_prefix("localhost").filter(|path| path.starts_with('/'))? };
    let mut bytes = Vec::new();
    let mut input = path.bytes();
    while let Some(ch) = input.next() {
        if ch == b'%' {
            let hi = (input.next()? as char).to_digit(16)?;
            let lo = (input.next()? as char).to_digit(16)?;
            bytes.push((hi * 16 + lo) as u8);
        } else { bytes.push(ch); }
    }
    String::from_utf8(bytes).ok()
}


fn relative(value: &str, root: &Path) -> Option<String> {
    let decoded;
    let path = if value.starts_with("file://") {
        decoded = unescape_uri(value)?;
        Path::new(&decoded)
    } else if let Some(suffix) = value.strip_prefix("~/") {
        decoded = PathBuf::from(std::env::var_os("HOME")?).join(suffix).to_str()?.to_string();
        Path::new(&decoded)
    } else { Path::new(value) };
    if !path.is_absolute() { return None; }
    let path = path.canonicalize().ok()?;
    let root = root.canonicalize().ok()?;
    let mut result = path.strip_prefix(root).ok()?.to_str()?.to_string();
    if result.is_empty() { result.push('.'); }
    if path.is_dir() { result.push('/'); }
    Some(result)
}

fn escaped(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t")
}

/// A shell-quoted/escaped token and its original byte span. This is only decoding:
/// dollar signs, command substitutions, wildcards, and tildes are never executed.
fn tokens(text: &str) -> Vec<(usize, usize, String)> {
    let mut chars = text.char_indices().peekable();
    let mut result = Vec::new();
    while let Some((start, ch)) = chars.next() {
        if ch.is_whitespace() { continue; }
        let mut token = String::new();
        let mut quote = None;
        let mut current = Some((start, ch));
        let mut end = start;
        let mut complete = true;
        while let Some((offset, ch)) = current {
            end = offset + ch.len_utf8();
            match (quote, ch) {
                (None, '\'' | '"') => quote = Some(ch),
                (Some(q), c) if q == c => quote = None,
                (None | Some('"'), '\\') => match chars.next() {
                    Some((offset, next)) => { token.push(next); end = offset + next.len_utf8(); }
                    None => { complete = false; break; }
                },
                _ => token.push(ch),
            }
            if chars.peek().is_none_or(|(_, ch)| quote.is_none() && ch.is_whitespace()) { break; }
            current = chars.next();
        }
        if quote.is_none() && complete { result.push((start, end, token)); }
    }
    result
}

/// Convert only existing absolute local file/directory arguments inside the project.
/// Other text (including paths outside the project) is kept byte-for-byte.
pub fn normalize(text: &str, root: &Path) -> String {
    let mut result = String::new();
    let mut copied = 0;
    for (start, end, value) in tokens(text) {
        if let Some(path) = relative(&value, root) {
            result.push_str(&text[copied..start]);
            result.push('"');
            result.push_str(&escaped(&path));
            result.push('"');
            copied = end;
        }
    }
    result.push_str(&text[copied..]);
    result
}

fn inside_string(prefix: &str) -> bool {
    let mut quoted = false;
    let mut escape = false;
    for ch in prefix.chars() {
        if escape { escape = false; }
        else if ch == '\\' && quoted { escape = true; }
        else if ch == '"' { quoted = !quoted; }
    }
    quoted
}

pub fn paste(text: &str, prefix: &str, root: &Path) -> String {
    // Some terminals drop an unquoted absolute path with literal spaces.
    let parsed = tokens(text);
    let single = relative(text.trim(), root).or_else(|| {
        if parsed.len() == 1 { relative(&parsed[0].2, root) } else { None }
    });
    if let Some(path) = single {
        if inside_string(prefix) { escaped(&path) }
        else { format!("\"{}\" ", escaped(&path)) }
    } else {
        normalize(text, root).replace("\r\n", " ").replace(['\n', '\r'], " ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("forgeflow-drop-test-{}", uuid::Uuid::now_v7()));
            std::fs::create_dir_all(root.join("shots")).unwrap();
            std::fs::write(root.join("shots/face copy.webp"), "test").unwrap();
            std::fs::write(root.join("shots/顔.webp"), "test").unwrap();
            Self(root)
        }
    }
    impl Drop for Fixture { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
    #[test]
    fn converts_shell_quoted_escaped_and_uri_drops() {
        let f = Fixture::new();
        let path = f.0.join("shots/face copy.webp").to_string_lossy().into_owned();
        for value in [format!("'{path}'"), format!("\"{path}\""), path.replace(' ', "\\ "), format!("file://{}", path.replace(' ', "%20"))] {
            assert_eq!(paste(&value, "; ", &f.0), "\"shots/face copy.webp\" ", "{value}");
        }
        assert_eq!(paste(&path, "; ", &f.0), "\"shots/face copy.webp\" ");
        assert_eq!(paste(&path, "; \"", &f.0), "shots/face copy.webp");
    }
    #[test]
    fn converts_multiple_drops_and_paths_in_programs_without_changing_other_text() {
        let f = Fixture::new();
        let first = f.0.join("shots/face copy.webp");
        let second = f.0.join("shots/顔.webp");
        let line = format!("; '{}' '{}' \".tmp/\" swap-face", first.display(), second.display());
        assert_eq!(normalize(&line, &f.0), "; \"shots/face copy.webp\" \"shots/顔.webp\" \".tmp/\" swap-face");
        let prose = "Explain 'quoted words' and /no/such/path without changing anything";
        assert_eq!(normalize(prose, &f.0), prose);
        assert_eq!(normalize("\"unfinished /some/path", &f.0), "\"unfinished /some/path");
    }
    #[test]
    fn keeps_external_paths_and_remote_uris_unchanged_and_handles_directories() {
        let f = Fixture::new();
        assert_eq!(normalize("/etc/passwd file://remote/etc/passwd", &f.0), "/etc/passwd file://remote/etc/passwd");
        assert_eq!(paste(f.0.join("shots").to_str().unwrap(), "; ", &f.0), "\"shots/\" ");
        let uri = format!("file://localhost{}/shots/%E9%A1%94.webp", f.0.display());
        assert_eq!(paste(&uri, "; ", &f.0), "\"shots/顔.webp\" ");
    }
}

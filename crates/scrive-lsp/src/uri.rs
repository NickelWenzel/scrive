//! URI identity: one normalized form per document, used to register, look up and send.
//!
//! Servers and hosts spell the same file differently (`file:///C%3A/x`, `file:///c:/x`,
//! `file://localhost/x`), and lsp-types' `Uri` compares raw strings. Every URI is normalized once
//! into a [`Key`] and compared only as a key.

use core::fmt;
use std::str::FromStr;

use lsp_types::Uri;

/// A normalized URI, made by [`Key::new`]. Two spellings of one `file:` path produce equal keys.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key(Uri);

impl Key {
    /// Normalizes `uri` into a key.
    ///
    /// For `file:` URIs the scheme and a leading drive letter are lower-cased, a `localhost`
    /// authority is dropped, and the path is re-encoded with one fixed RFC 3986 set: escapes
    /// decode, except those that decode to `/`, `%`, `?` or `#` and bytes that are not UTF-8, and
    /// everything outside the set (all non-ASCII included) is percent-encoded in upper-case hex.
    /// The query and fragment are kept as they are. URIs of other schemes pass through unchanged.
    ///
    /// Normalizing a key's URI again yields the same key.
    #[must_use]
    pub fn new(uri: &Uri) -> Self {
        let raw = uri.as_str();
        let Some((scheme, rest)) = raw.split_once(':') else {
            return Key(uri.clone());
        };
        if !scheme.eq_ignore_ascii_case("file") {
            return Key(uri.clone());
        }
        let hier = without_query(rest);
        let tail = &rest[hier.len()..];
        let mut out = String::from("file:");
        let path = match hier.strip_prefix("//") {
            Some(after) => {
                let (authority, path) = after.split_at(after.find('/').unwrap_or(after.len()));
                out.push_str("//");
                if !authority.eq_ignore_ascii_case("localhost") {
                    out.push_str(authority);
                }
                path
            }
            None => hier,
        };
        out.push_str(&normalize_path(path));
        out.push_str(tail);
        // The output holds only characters fluent-uri accepts in a path, so the fallback never
        // runs; it keeps the function total without a panic.
        Uri::from_str(&out).map_or_else(|_| Key(uri.clone()), Key)
    }

    /// The normalized URI, as sent to a server.
    #[must_use]
    pub fn uri(&self) -> &Uri {
        &self.0
    }

    /// The normalized URI text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// The decoded file-system path of a `file:` key, `None` for other schemes. A drive letter
    /// loses its leading slash (`c:/x`), and a host other than `localhost` keeps its UNC form
    /// (`//host/share`).
    pub(crate) fn file_path(&self) -> Option<String> {
        let rest = without_query(self.as_str().strip_prefix("file:")?);
        let path = rest
            .strip_prefix("//")
            .filter(|path| path.starts_with('/'))
            .unwrap_or(rest);
        let path = decode(path);
        let bytes = path.as_bytes();
        let drive = matches!(bytes, [b'/', letter, b':', ..] if letter.is_ascii_alphabetic());
        Some(if drive { path[1..].to_owned() } else { path })
    }

    /// The decoded last non-empty path segment, or the whole URI text when there is none.
    pub(crate) fn name(&self) -> String {
        without_query(self.as_str())
            .rsplit('/')
            .find(|segment| !segment.is_empty())
            .map_or_else(|| self.as_str().to_owned(), decode)
    }

    /// The file-system path of a `file:` key, `None` for other schemes. A drive letter comes
    /// back as `c:/…` and a remote host as `//host/share/…`.
    #[cfg(not(target_family = "wasm"))]
    #[must_use]
    pub fn to_path(&self) -> Option<std::path::PathBuf> {
        self.file_path().map(std::path::PathBuf::from)
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The `file:` URI of the absolute `path`, normalized, with every byte outside RFC 3986's
/// unreserved set, `/` and `:` percent-encoded. `None` for a relative path or one that is not
/// UTF-8.
#[cfg(not(target_family = "wasm"))]
#[must_use]
pub fn from_path(path: &std::path::Path) -> Option<Uri> {
    if !path.is_absolute() {
        return None;
    }
    let mut path = path.to_str()?.to_owned();
    if cfg!(windows) {
        path = path.replace('\\', "/");
    }
    if !path.starts_with('/') {
        // A drive-letter path: `file:///C:/…`.
        path.insert(0, '/');
    }
    let mut text = String::from("file://");
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/:".contains(&byte) {
            text.push(char::from(byte));
        } else {
            push_escape(&mut text, byte);
        }
    }
    Uri::from_str(&text).ok().map(|uri| Key::new(&uri).0)
}

/// `text` up to its query or fragment.
fn without_query(text: &str) -> &str {
    &text[..text.find(['?', '#']).unwrap_or(text.len())]
}

/// Percent-decodes `text`. Decoded bytes that are not UTF-8 become U+FFFD.
fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match hex_pair(bytes.get(i + 1..i + 3)).filter(|_| bytes[i] == b'%') {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Decodes and re-encodes one path, then lowers a leading drive letter. `raw` is ASCII, since it
/// comes from a parsed URI.
fn normalize_path(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = String::with_capacity(raw.len());
    // Consecutive escapes decode together, so a multi-byte UTF-8 character survives as one.
    let mut run = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if let Some(byte) = hex_pair(bytes.get(i + 1..i + 3)) {
                if matches!(byte, b'/' | b'%' | b'?' | b'#') {
                    flush(&mut run, &mut out);
                    push_escape(&mut out, byte);
                } else {
                    run.push(byte);
                }
                i += 3;
                continue;
            }
        }
        flush(&mut run, &mut out);
        push_byte(&mut out, bytes[i]);
        i += 1;
    }
    flush(&mut run, &mut out);
    lower_drive_letter(out)
}

/// Emits decoded bytes: valid UTF-8 through the fixed set, invalid sequences as their escapes.
fn flush(run: &mut Vec<u8>, out: &mut String) {
    for chunk in run.utf8_chunks() {
        for &byte in chunk.valid().as_bytes() {
            push_byte(out, byte);
        }
        for &byte in chunk.invalid() {
            push_escape(out, byte);
        }
    }
    run.clear();
}

/// The fixed path set: unreserved characters, sub-delims, `:`, `@` and `/` stay literal, and
/// every other byte is escaped.
fn push_byte(out: &mut String, byte: u8) {
    if byte.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@/".contains(&byte) {
        out.push(char::from(byte));
    } else {
        push_escape(out, byte);
    }
}

fn push_escape(out: &mut String, byte: u8) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    out.push('%');
    out.push(char::from(HEX[usize::from(byte >> 4)]));
    out.push(char::from(HEX[usize::from(byte & 0xF)]));
}

/// The byte two hex digits spell, or `None` when `pair` is not exactly two hex digits.
fn hex_pair(pair: Option<&[u8]>) -> Option<u8> {
    let &[high, low] = pair? else { return None };
    let digit = |b: u8| char::from(b).to_digit(16);
    Some((digit(high)? * 16 + digit(low)?) as u8)
}

/// `/C:/…` → `/c:/…`. Windows drive letters are case-insensitive, and VS Code sends them
/// lower-case, so servers built against it do too.
fn lower_drive_letter(mut path: String) -> String {
    let bytes = path.as_bytes();
    if bytes.len() >= 3
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
        && bytes.get(3).is_none_or(|&b| b == b'/')
    {
        path[1..2].make_ascii_lowercase();
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every normalization fixture, as `(input, normalized)`.
    const FIXTURES: &[(&str, &str)] = &[
        (
            "file:///C%3A/Users/a%40b/x%2By.rs",
            "file:///c:/Users/a@b/x+y.rs",
        ),
        ("file:///tmp/a%20b.rs", "file:///tmp/a%20b.rs"),
        ("file://localhost/tmp/a.rs", "file:///tmp/a.rs"),
        ("file://LOCALHOST/tmp/a.rs", "file:///tmp/a.rs"),
        ("FILE:///C:/x.rs", "file:///c:/x.rs"),
        (
            "file:///tmp/a%2fb%25c%3Fd%23e",
            "file:///tmp/a%2Fb%25c%3Fd%23e",
        ),
        ("file:///tmp/%FF.rs", "file:///tmp/%FF.rs"),
        ("file:///tmp/%c3%a9.rs", "file:///tmp/%C3%A9.rs"),
        ("untitled:Untitled-1", "untitled:Untitled-1"),
        ("https://Example.com/A%3a", "https://Example.com/A%3a"),
    ];

    fn key(text: &str) -> Key {
        Key::new(&Uri::from_str(text).expect("fixture URI parses"))
    }

    fn assert_normalizes(pairs: &[(&str, &str)]) {
        for &(input, expected) in pairs {
            assert_eq!(key(input).as_str(), expected, "{input} normalizes");
        }
    }

    /// Escapes of ordinary characters decode, so `%3A`, `%40` and `%2B` equal their characters.
    #[test]
    fn ordinary_file_escapes_decode() {
        assert_normalizes(&FIXTURES[0..2]);
    }

    /// `file://localhost/x` names the same file as `file:///x`, in any case.
    #[test]
    fn localhost_authority_is_dropped() {
        assert_normalizes(&FIXTURES[2..4]);
    }

    /// The scheme and a Windows drive letter compare case-insensitively.
    #[test]
    fn scheme_and_drive_letter_are_lowercased() {
        assert_normalizes(&FIXTURES[4..5]);
    }

    /// Decoding `/ % ? #` would change the path's structure, and bytes that are not UTF-8 have no
    /// character to decode to, so those escapes stay (in upper-case hex).
    #[test]
    fn reserved_and_invalid_utf8_escapes_are_kept() {
        assert_normalizes(&FIXTURES[5..7]);
    }

    /// Non-ASCII characters are always sent escaped, with one hex case.
    #[test]
    fn non_ascii_is_percent_encoded_in_upper_case() {
        assert_normalizes(&FIXTURES[7..8]);
    }

    /// Only `file:` URIs have a normalization; the rest are compared as sent.
    #[test]
    fn non_file_uris_pass_through() {
        assert_normalizes(&FIXTURES[8..]);
    }

    /// `rootPath` is the decoded local path; a Windows drive path has no leading slash.
    #[test]
    fn file_path_and_name_are_decoded() {
        let unix = key("file:///work/my%20proj/");
        assert_eq!(
            unix.file_path().as_deref(),
            Some("/work/my proj/"),
            "a unix path decodes"
        );
        assert_eq!(
            unix.name(),
            "my proj",
            "the name is the last non-empty segment"
        );
        let windows = key("file:///C%3A/work");
        assert_eq!(
            windows.file_path().as_deref(),
            Some("c:/work"),
            "a drive path loses its slash"
        );
        assert_eq!(
            key("untitled:x").file_path(),
            None,
            "only file URIs have a path"
        );
    }

    /// Everything `Key::new` emits parses as a `Uri`, and normalizing it again changes nothing.
    #[test]
    fn normalized_uris_round_trip_through_uri_from_str() {
        for &(input, _) in FIXTURES {
            let normalized = key(input);
            let reparsed = Uri::from_str(normalized.as_str()).expect("normalized URI parses");
            assert_eq!(
                Key::new(&reparsed),
                normalized,
                "{input} normalizes idempotently"
            );
        }
    }

    /// Spaces and non-ASCII are percent-encoded, and the result is already in normal form, so
    /// the server sees the URI the host built.
    #[cfg(unix)]
    #[test]
    fn from_path_percent_encodes_and_is_already_normalized() {
        let uri = from_path(std::path::Path::new("/tmp/a dir/é.rs")).expect("an absolute path");
        assert_eq!(
            uri.as_str(),
            "file:///tmp/a%20dir/%C3%A9.rs",
            "the path is encoded"
        );
        assert_eq!(
            Key::new(&uri).as_str(),
            uri.as_str(),
            "normalizing changes nothing"
        );
    }

    /// A relative path names no file on its own.
    #[cfg(not(target_family = "wasm"))]
    #[test]
    fn from_path_refuses_a_relative_path() {
        assert_eq!(
            from_path(std::path::Path::new("rel/x.rs")),
            None,
            "a relative path has no URI"
        );
    }

    /// A path survives the round trip through its URI.
    #[cfg(unix)]
    #[test]
    fn to_path_inverts_from_path() {
        let path = std::path::Path::new("/tmp/a dir/é.rs");
        let uri = from_path(path).expect("an absolute path");
        assert_eq!(
            Key::new(&uri).to_path().as_deref(),
            Some(path),
            "the path comes back"
        );
    }

    /// Only `file:` keys name a path.
    #[cfg(not(target_family = "wasm"))]
    #[test]
    fn non_file_keys_have_no_path() {
        assert_eq!(key("untitled:x").to_path(), None, "an untitled key has no path");
    }
}

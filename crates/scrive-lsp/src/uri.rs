//! URI identity: one normalized form per document, used to register, look up and send.
//!
//! Servers and hosts spell the same file differently (`file:///C%3A/x`, `file:///c:/x`,
//! `file://localhost/x`), and lsp-types' `Uri` compares raw strings. Every URI is normalized once
//! into a [`Key`] and compared only as a key.

use core::fmt;
use std::str::FromStr;

use lsp_types::Uri;

/// A normalized URI, made by [`normalize`]. Two spellings of one `file:` path produce equal keys.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key(Uri);

impl Key {
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
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Normalizes `uri` into a [`Key`].
///
/// For `file:` URIs the scheme and a leading drive letter are lower-cased, a `localhost`
/// authority is dropped, and the path is re-encoded with one fixed RFC 3986 set: escapes decode,
/// except those that decode to `/`, `%`, `?` or `#` and bytes that are not UTF-8, and everything
/// outside the set (all non-ASCII included) is percent-encoded in upper-case hex. The query and
/// fragment are kept as they are. URIs of other schemes pass through unchanged.
///
/// Normalizing a key's URI again yields the same key.
#[must_use]
pub fn normalize(uri: &Uri) -> Key {
    let raw = uri.as_str();
    let Some((scheme, rest)) = raw.split_once(':') else {
        return Key(uri.clone());
    };
    if !scheme.eq_ignore_ascii_case("file") {
        return Key(uri.clone());
    }
    let (hier, tail) = rest.split_at(rest.find(['?', '#']).unwrap_or(rest.len()));
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
    // The output holds only characters fluent-uri accepts in a path, so the fallback never runs;
    // it keeps the function total without a panic.
    Uri::from_str(&out).map_or_else(|_| Key(uri.clone()), Key)
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
        normalize(&Uri::from_str(text).expect("fixture URI parses"))
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

    /// Everything `normalize` emits parses as a `Uri`, and normalizing it again changes nothing.
    #[test]
    fn normalized_uris_round_trip_through_uri_from_str() {
        for &(input, _) in FIXTURES {
            let normalized = key(input);
            let reparsed = Uri::from_str(normalized.as_str()).expect("normalized URI parses");
            assert_eq!(
                normalize(&reparsed),
                normalized,
                "{input} normalizes idempotently"
            );
        }
    }
}

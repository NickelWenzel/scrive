//! Signature help converted to scrive's one-line signature box.

use core::ops::Range;

use lsp_types::{ParameterInformation, ParameterLabel, SignatureHelp};
use scrive_core::SignatureInfo;

use crate::{markdown, Encoding};

/// What a pending signature request asked.
#[derive(Clone, Debug)]
pub(crate) struct Query {
    /// The editor's call identity: the offset of the innermost `(` around the caret.
    pub(crate) call: Option<u32>,
    /// The caret the request's position was computed from.
    pub(crate) caret: u32,
}

/// The active signature, or `None` when the server offers none.
pub(crate) fn convert(help: SignatureHelp) -> Option<SignatureInfo> {
    let last = help.signatures.len().checked_sub(1)?;
    let index = (help.active_signature.unwrap_or(0) as usize).min(last);
    let shared = help.active_parameter;
    let signature = help.signatures.into_iter().nth(index)?;
    let params = parameters(
        &signature.label,
        signature.parameters.as_deref().unwrap_or_default(),
    );
    // The per-signature field (LSP 3.16) is precise; the top-level one is shared by every
    // signature and only a fallback.
    let active = signature
        .active_parameter
        .or(shared)
        .unwrap_or(0)
        .min(params.len().saturating_sub(1) as u32);
    Some(SignatureInfo {
        doc: signature
            .documentation
            .as_ref()
            .map(markdown::documentation),
        label: signature.label,
        params,
        active,
    })
}

/// Byte ranges of each parameter within `label`, one per parameter so the indices stay aligned
/// with the active parameter. A label that is not found is an empty range where the search
/// stood.
fn parameters(label: &str, parameters: &[ParameterInformation]) -> Vec<Range<u32>> {
    // The search starts after the first `(`, so a parameter named like the function (`x(x)`)
    // is found in the parameter list, not in the name.
    let mut cursor = label.find('(').map_or(0, |open| open + 1);
    parameters
        .iter()
        .map(|parameter| {
            let range = match &parameter.label {
                ParameterLabel::Simple(text) if !text.is_empty() => {
                    match label[cursor..].find(text.as_str()) {
                        Some(at) => cursor + at..cursor + at + text.len(),
                        None => cursor..cursor,
                    }
                }
                ParameterLabel::Simple(_) => cursor..cursor,
                // Label offsets are UTF-16 code units whatever position encoding was negotiated
                // (LSP §textDocument_signatureHelp, `ParameterInformation.label`).
                ParameterLabel::LabelOffsets([start, end]) => {
                    let end = Encoding::Utf16.bytes([label], *end) as usize;
                    let start = (Encoding::Utf16.bytes([label], *start) as usize).min(end);
                    start..end
                }
            };
            cursor = range.end;
            range.start as u32..range.end as u32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::*;

    fn converted(help: Value) -> Option<SignatureInfo> {
        convert(serde_json::from_value(help).expect("fixture decodes"))
    }

    /// The parameter ranges of a one-signature reply with `label` and `parameters`.
    fn ranges(label: &str, parameters: Value) -> Vec<Range<u32>> {
        let parameters: Vec<Value> = parameters
            .as_array()
            .expect("an array of labels")
            .iter()
            .map(|label| json!({"label": label}))
            .collect();
        converted(json!({"signatures": [{"label": label, "parameters": parameters}]}))
            .expect("one signature")
            .params
    }

    fn active(signature: Value, top: Option<u32>) -> u32 {
        let mut help = json!({"signatures": [signature]});
        if let Some(top) = top {
            help["activeParameter"] = json!(top);
        }
        converted(help).expect("one signature").active
    }

    /// No signature means no box.
    #[test]
    fn empty_signatures_convert_to_none() {
        assert!(
            converted(json!({"signatures": []})).is_none(),
            "an empty reply closes the box"
        );
    }

    /// An active signature past the end selects the last one.
    #[test]
    fn active_signature_is_clamped() {
        let info = converted(json!({
            "signatures": [{"label": "f()"}, {"label": "g()"}],
            "activeSignature": 7,
        }))
        .expect("a signature");
        assert_eq!(info.label, "g()", "the last signature is active");
    }

    /// The signature's own active parameter overrides the shared one.
    #[test]
    fn per_signature_active_parameter_wins() {
        let signature = json!({"label": "f(a, b)", "parameters": [{"label": "a"}, {"label": "b"}], "activeParameter": 1});
        assert_eq!(
            active(signature, Some(0)),
            1,
            "the per-signature field wins"
        );
        let signature = json!({"label": "f(a, b)", "parameters": [{"label": "a"}, {"label": "b"}]});
        assert_eq!(
            active(signature.clone(), Some(1)),
            1,
            "the shared field is the fallback"
        );
        assert_eq!(active(signature, None), 0, "absent means the first");
    }

    /// An active parameter past the end is the last parameter, and `0` without parameters.
    #[test]
    fn active_parameter_is_clamped_to_the_last_parameter() {
        let two = json!({"label": "f(a, b)", "parameters": [{"label": "a"}, {"label": "b"}], "activeParameter": 5});
        assert_eq!(active(two, None), 1, "clamped to the last parameter");
        let none = json!({"label": "f()", "activeParameter": 3});
        assert_eq!(active(none, None), 0, "no parameters is 0");
    }

    /// A parameter named like the function is found in the parameter list.
    #[test]
    fn simple_labels_are_found_after_the_open_paren() {
        assert_eq!(ranges("x(x)", json!(["x"])), vec![2..3], "found after `(`");
        assert_eq!(
            ranges("a b", json!(["b"])),
            vec![2..3],
            "without `(` the search starts at 0"
        );
        assert_eq!(
            ranges("foo(a: i32, b: i32)", json!(["a: i32", "b: i32"])),
            vec![4..10, 12..18],
            "each parameter is found"
        );
    }

    /// A repeated label is found once per parameter, left to right.
    #[test]
    fn repeated_simple_labels_are_found_in_order() {
        assert_eq!(
            ranges("max(x, x)", json!(["x", "x"])),
            vec![4..5, 7..8],
            "the second search starts after the first match"
        );
    }

    /// A label missing from the signature keeps its slot, empty, and the search goes on.
    #[test]
    fn missing_simple_label_is_an_empty_range_at_the_cursor() {
        assert_eq!(
            ranges("f(a, b)", json!(["a", "zz", "b"])),
            vec![2..3, 3..3, 5..6],
            "the missing label is empty where the search stood"
        );
    }

    /// Label offsets count UTF-16 code units, not bytes.
    #[test]
    fn label_offsets_are_utf16() {
        assert_eq!(
            ranges("f(é: 😀)", json!([[5, 7]])),
            vec![6..10],
            "the emoji spans units 5..7"
        );
    }

    /// Offsets inside a surrogate pair snap to its start, past the end clamp, and inverted ones
    /// collapse to the end.
    #[test]
    fn label_offsets_snap_clamp_and_collapse() {
        for (offsets, expected) in [([6, 7], 6..10), ([5, 99], 6..11), ([7, 5], 6..6)] {
            assert_eq!(
                ranges("f(é: 😀)", json!([offsets])),
                vec![expected],
                "{offsets:?}"
            );
        }
    }

    /// The box shows documentation as plain text.
    #[test]
    fn signature_documentation_is_plain_text() {
        let info = converted(json!({"signatures": [{
            "label": "f()",
            "documentation": {"kind": "markdown", "value": "**x**"},
        }]}))
        .expect("a signature");
        assert_eq!(info.doc.as_deref(), Some("x"), "markdown is lowered");
    }
}

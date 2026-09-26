//! Server diagnostics converted to scrive's.

use lsp_types::{DiagnosticSeverity, NumberOrString};
use scrive_core::{Diagnostic, Severity, Snapshot};

use crate::Encoding;

/// Converts a published set against the snapshot the server saw.
pub(crate) fn convert(
    encoding: Encoding,
    snapshot: &Snapshot,
    diagnostics: &[lsp_types::Diagnostic],
) -> Vec<Diagnostic> {
    diagnostics
        .iter()
        .map(|diagnostic| {
            let mut out = Diagnostic::new(
                encoding.span(snapshot, diagnostic.range),
                severity(diagnostic.severity),
                diagnostic.message.clone(),
            );
            out.code = diagnostic.code.as_ref().map(|code| match code {
                NumberOrString::Number(n) => n.to_string(),
                NumberOrString::String(s) => s.clone(),
            });
            out
        })
        .collect()
}

/// The spec leaves a missing severity to the client. Reading it (or an unknown value) as an
/// error is the safe side: hiding a real error as a hint is worse than the reverse.
fn severity(severity: Option<DiagnosticSeverity>) -> Severity {
    match severity {
        Some(DiagnosticSeverity::WARNING) => Severity::Warning,
        Some(DiagnosticSeverity::INFORMATION) => Severity::Info,
        Some(DiagnosticSeverity::HINT) => Severity::Hint,
        _ => Severity::Error,
    }
}

#[cfg(test)]
mod tests {
    use scrive_core::Document;
    use serde_json::{json, Value};

    use super::*;

    fn converted(fields: Value) -> Diagnostic {
        let mut diagnostic = json!({
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}},
            "message": "m",
        });
        if let (Some(diagnostic), Some(fields)) = (diagnostic.as_object_mut(), fields.as_object()) {
            diagnostic.extend(fields.clone());
        }
        let diagnostic: lsp_types::Diagnostic =
            serde_json::from_value(diagnostic).expect("fixture diagnostic decodes");
        let snapshot = Document::new("ab").expect("fixture loads").snapshot();
        let [out] = convert(Encoding::Utf16, &snapshot, &[diagnostic])
            .try_into()
            .expect("one diagnostic in, one out");
        out
    }

    /// Only the three non-error severities keep their meaning; absent and unknown are errors.
    #[test]
    fn missing_or_unknown_severity_maps_to_error() {
        for (fields, expected) in [
            (json!({}), Severity::Error),
            (json!({"severity": 1}), Severity::Error),
            (json!({"severity": 2}), Severity::Warning),
            (json!({"severity": 3}), Severity::Info),
            (json!({"severity": 4}), Severity::Hint),
            (json!({"severity": 9}), Severity::Error),
        ] {
            assert_eq!(
                converted(fields.clone()).severity,
                expected,
                "{fields} maps"
            );
        }
    }

    /// Codes of either wire type read as text; an absent code stays absent.
    #[test]
    fn numeric_and_string_codes_become_strings() {
        for (fields, expected) in [
            (json!({"code": 42}), Some("42")),
            (json!({"code": "E0308"}), Some("E0308")),
            (json!({}), None),
        ] {
            assert_eq!(
                converted(fields.clone()).code.as_deref(),
                expected,
                "{fields} converts"
            );
        }
    }
}

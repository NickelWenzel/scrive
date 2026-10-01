//! Inlay hints: a `textDocument/inlayHint` answer converted to scrive's hint model, and the set a
//! document keeps so hint gestures can be answered from what the server sent.

use std::collections::HashMap;
use std::ops::Range;

use lsp_types::{InlayHintKind, InlayHintLabel, Position};
use scrive_core::{intel, Revision, Snapshot};
use serde_json::Value;

use crate::Encoding;

/// The hints of one answer, kept by key for the gestures on them.
#[derive(Debug)]
pub(crate) struct Set {
    revision: Revision,
    /// In server order.
    hints: Vec<Stored>,
}

/// One installed hint as the server sent it.
#[derive(Debug)]
struct Stored {
    key: intel::inlay::Key,
    hint: lsp_types::InlayHint,
}

/// One entry that decoded and lies in the clipped span, at its offset in the request snapshot.
pub(crate) struct Fetched {
    offset: u32,
    hint: lsp_types::InlayHint,
}

impl Set {
    /// The set for an answer at `snapshot`, and its hints for the editor, in server order. A hint
    /// matching one of `previous` by position, kind and label texts keeps that hint's key, first
    /// unused match first, so repeated identical hints keep theirs in order; every other hint
    /// gets the next key from `counter`. `previous` must be at the same revision.
    pub(crate) fn install(
        snapshot: &Snapshot,
        fetched: Vec<Fetched>,
        previous: Option<Set>,
        counter: &mut u64,
    ) -> (Set, Vec<intel::inlay::Placed>) {
        let mut reusable: HashMap<Position, Vec<Stored>> = HashMap::new();
        for stored in previous.into_iter().flat_map(|set| set.hints) {
            reusable
                .entry(stored.hint.position)
                .or_default()
                .push(stored);
        }
        let mut hints = Vec::with_capacity(fetched.len());
        let mut placed = Vec::with_capacity(fetched.len());
        for fetched in fetched {
            let key = reusable
                .get_mut(&fetched.hint.position)
                .and_then(|same| {
                    let at = same
                        .iter()
                        .position(|stored| same_hint(&stored.hint, &fetched.hint))?;
                    Some(same.remove(at).key)
                })
                .unwrap_or_else(|| {
                    *counter += 1;
                    intel::inlay::Key::new(*counter)
                });
            if let Some(hint) = convert(&fetched, key) {
                placed.push(hint);
                hints.push(Stored {
                    key,
                    hint: fetched.hint,
                });
            }
        }
        let set = Set {
            revision: snapshot.revision(),
            hints,
        };
        (set, placed)
    }

    /// The revision the set was fetched at.
    pub(crate) fn revision(&self) -> Revision {
        self.revision
    }
}

/// The entries of an answer over `span` that decode and lie within one line of it, in server
/// order. An entry that does not decode is skipped alone. Hints past the last line are dropped
/// rather than clamped to the document end.
pub(crate) fn decode(
    encoding: Encoding,
    snapshot: &Snapshot,
    span: Range<u32>,
    entries: Vec<Value>,
) -> Vec<Fetched> {
    // The spec allows hints outside the requested range; clipping keeps the editor's store
    // bounded by its window.
    let first = snapshot
        .offset_to_point(span.start.min(snapshot.len()))
        .row
        .saturating_sub(1);
    let last = snapshot
        .offset_to_point(span.end.min(snapshot.len()))
        .row
        .saturating_add(1);
    entries
        .into_iter()
        .filter_map(|raw| {
            let hint: lsp_types::InlayHint = serde_json::from_value(raw).ok()?;
            let line = hint.position.line;
            if line < first || line > last || line >= snapshot.line_count() {
                return None;
            }
            let offset = encoding.offset(snapshot, hint.position);
            Some(Fetched { offset, hint })
        })
        .collect()
}

/// `fetched` as scrive's hint under `key`, or `None` when core rejects its label.
fn convert(fetched: &Fetched, key: intel::inlay::Key) -> Option<intel::inlay::Placed> {
    let hint = &fetched.hint;
    let label: Vec<intel::inlay::Part> = match &hint.label {
        InlayHintLabel::String(text) => {
            vec![intel::inlay::Part::new(
                text.clone(),
                intel::inlay::Link::None,
            )]
        }
        InlayHintLabel::LabelParts(parts) => parts
            .iter()
            .map(|part| {
                let link = if part.location.is_some() {
                    intel::inlay::Link::Jumps
                } else {
                    intel::inlay::Link::None
                };
                intel::inlay::Part::new(part.value.clone(), link)
            })
            .collect(),
    };
    let kind = match hint.kind {
        Some(InlayHintKind::TYPE) => intel::inlay::Kind::Type,
        Some(InlayHintKind::PARAMETER) => intel::inlay::Kind::Parameter,
        _ => intel::inlay::Kind::Other,
    };
    let left = hint.padding_left == Some(true);
    let right = hint.padding_right == Some(true);
    let placement = match (kind, left, right) {
        (intel::inlay::Kind::Type, ..) => intel::inlay::Placement::Suffix,
        (intel::inlay::Kind::Parameter, ..) => intel::inlay::Placement::Prefix,
        // Padding faces away from the token the hint annotates (Zed's `hint_position_and_bias`).
        (intel::inlay::Kind::Other, false, true) => intel::inlay::Placement::Prefix,
        (intel::inlay::Kind::Other, true, false) => intel::inlay::Placement::Suffix,
        (intel::inlay::Kind::Other, ..) => intel::inlay::Placement::Auto,
    };
    let texts = label_texts(&hint.label);
    // A label that already carries the space at an edge gets no padding there, so its cells
    // match its text.
    let padding = intel::inlay::Padding {
        left: left
            && !texts
                .first()
                .is_some_and(|t| t.starts_with(char::is_whitespace)),
        right: right
            && !texts
                .last()
                .is_some_and(|t| t.ends_with(char::is_whitespace)),
    };
    let insert = if hint
        .text_edits
        .as_ref()
        .is_some_and(|edits| !edits.is_empty())
    {
        intel::inlay::Insert::Available
    } else {
        intel::inlay::Insert::Unavailable
    };
    let hint = intel::inlay::Hint::new(kind, label, key)
        .ok()?
        .padding(padding)
        .insert(insert)
        .placement(placement);
    Some(intel::inlay::Placed::new(fetched.offset, hint))
}

fn label_texts(label: &InlayHintLabel) -> Vec<&str> {
    match label {
        InlayHintLabel::String(text) => vec![text.as_str()],
        InlayHintLabel::LabelParts(parts) => parts.iter().map(|part| part.value.as_str()).collect(),
    }
}

/// Same position, kind and label texts: the same hint, refetched.
fn same_hint(a: &lsp_types::InlayHint, b: &lsp_types::InlayHint) -> bool {
    a.position == b.position && a.kind == b.kind && label_texts(&a.label) == label_texts(&b.label)
}

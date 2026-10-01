//! Inlay hints: a `textDocument/inlayHint` answer converted to scrive's hint model, and the set a
//! document keeps so hint gestures can be answered from what the server sent.

use std::collections::HashMap;
use std::ops::Range;

use lsp_types::{
    InlayHintKind, InlayHintLabel, InlayHintLabelPartTooltip, InlayHintTooltip, MarkupKind,
    Position,
};
use scrive_core::intel::hover::escape_markdown;
use scrive_core::{intel, Revision, Snapshot};
use serde_json::Value;

use crate::{markdown, uri, Encoding};

/// The hints of one answer, kept by key for the gestures on them.
#[derive(Debug)]
pub(crate) struct Set {
    /// What the hints' positions and edits convert against.
    snapshot: Snapshot,
    /// Every open document's synced revision when the hints were asked for.
    revisions: Vec<(uri::Key, Revision)>,
    /// In server order.
    hints: Vec<Stored>,
}

/// One installed hint as the server sent it.
#[derive(Debug)]
pub(crate) struct Stored {
    key: intel::inlay::Key,
    hint: lsp_types::InlayHint,
    /// Sent back verbatim by `inlayHint/resolve`: its `data` belongs to the server.
    raw: Value,
    /// A resolve reply has been absorbed.
    resolved: bool,
}

/// One entry that decoded and lies in the clipped span, at its offset in the request snapshot.
pub(crate) struct Fetched {
    offset: u32,
    hint: lsp_types::InlayHint,
    raw: Value,
}

impl Set {
    /// The set for an answer at `snapshot`, and its hints for the editor, in server order. A hint
    /// matching one of `previous` by position, kind and label texts keeps that hint's key, first
    /// unused match first, so repeated identical hints keep theirs in order; every other hint
    /// gets the next key from `counter`. `previous` must be at the same revision. `revisions`
    /// holds every open document's synced revision when the hints were asked for.
    pub(crate) fn install(
        snapshot: &Snapshot,
        revisions: Vec<(uri::Key, Revision)>,
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
                    raw: fetched.raw,
                    resolved: false,
                });
            }
        }
        let set = Set {
            snapshot: snapshot.clone(),
            revisions,
            hints,
        };
        (set, placed)
    }

    /// The revision the set was fetched at.
    pub(crate) fn revision(&self) -> Revision {
        self.snapshot.revision()
    }

    /// The text the hints were fetched against.
    pub(crate) fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// Every open document's synced revision when the hints were asked for.
    pub(crate) fn revisions(&self) -> &[(uri::Key, Revision)] {
        &self.revisions
    }

    /// The hint under `key`.
    pub(crate) fn get(&self, key: intel::inlay::Key) -> Option<&Stored> {
        self.hints.iter().find(|stored| stored.key == key)
    }

    /// The hint under `key`, to absorb a resolve into.
    pub(crate) fn get_mut(&mut self, key: intel::inlay::Key) -> Option<&mut Stored> {
        self.hints.iter_mut().find(|stored| stored.key == key)
    }
}

impl Stored {
    /// The tooltip for a gesture on `part`, lowered to the hover card's subset: the part's own,
    /// else the hint's. Whitespace-only tooltips count as none.
    pub(crate) fn tooltip(&self, part: u32) -> Option<String> {
        let own = self
            .part(part)
            .and_then(|part| part.tooltip.as_ref())
            .and_then(part_tooltip);
        own.or_else(|| self.hint.tooltip.as_ref().and_then(hint_tooltip))
    }

    /// The location of label part `part`, if it has one.
    pub(crate) fn location(&self, part: u32) -> Option<&lsp_types::Location> {
        self.part(part)?.location.as_ref()
    }

    /// The edits that insert the hint into the text; empty when it has none.
    pub(crate) fn text_edits(&self) -> &[lsp_types::TextEdit] {
        self.hint.text_edits.as_deref().unwrap_or_default()
    }

    /// Whether a resolve could still add a tooltip: the server keeps `data` on the hint for that.
    pub(crate) fn resolvable(&self) -> bool {
        !self.resolved && self.hint.data.is_some()
    }

    /// The hint as the server sent it, for `inlayHint/resolve`.
    pub(crate) fn raw(&self) -> &Value {
        &self.raw
    }

    /// Takes the tooltips of `resolved` that this hint lacks. The fetched label owns the parts: a
    /// resolve may restructure the label, so part tooltips are taken only from the same label.
    /// Links and text edits never change after install.
    pub(crate) fn absorb(&mut self, resolved: lsp_types::InlayHint) {
        self.resolved = true;
        if self.hint.tooltip.is_none() {
            self.hint.tooltip = resolved.tooltip;
        }
        if let (InlayHintLabel::LabelParts(mine), InlayHintLabel::LabelParts(theirs)) =
            (&mut self.hint.label, resolved.label)
        {
            let same = mine.len() == theirs.len()
                && mine.iter().zip(&theirs).all(|(a, b)| a.value == b.value);
            if same {
                for (mine, theirs) in mine.iter_mut().zip(theirs) {
                    if mine.tooltip.is_none() {
                        mine.tooltip = theirs.tooltip;
                    }
                }
            }
        }
    }

    /// Label part `part`; a string label has none.
    fn part(&self, part: u32) -> Option<&lsp_types::InlayHintLabelPart> {
        match &self.hint.label {
            InlayHintLabel::LabelParts(parts) => parts.get(part as usize),
            InlayHintLabel::String(_) => None,
        }
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
            let hint: lsp_types::InlayHint = serde_json::from_value(raw.clone()).ok()?;
            let line = hint.position.line;
            if line < first || line > last || line >= snapshot.line_count() {
                return None;
            }
            let offset = encoding.offset(snapshot, hint.position);
            Some(Fetched { offset, hint, raw })
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

/// The spec makes a bare string tooltip plain text.
fn hint_tooltip(tooltip: &InlayHintTooltip) -> Option<String> {
    match tooltip {
        InlayHintTooltip::String(text) => card(&MarkupKind::PlainText, text),
        InlayHintTooltip::MarkupContent(content) => card(&content.kind, &content.value),
    }
}

fn part_tooltip(tooltip: &InlayHintLabelPartTooltip) -> Option<String> {
    match tooltip {
        InlayHintLabelPartTooltip::String(text) => card(&MarkupKind::PlainText, text),
        InlayHintLabelPartTooltip::MarkupContent(content) => card(&content.kind, &content.value),
    }
}

/// `text` in the hover card's markdown subset, or `None` when nothing would show.
fn card(kind: &MarkupKind, text: &str) -> Option<String> {
    let markdown = match kind {
        MarkupKind::Markdown => markdown::to_hover(text),
        MarkupKind::PlainText => escape_markdown(text),
    };
    (!markdown.trim().is_empty()).then_some(markdown)
}

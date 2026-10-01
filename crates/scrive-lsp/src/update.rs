//! What the client hands back: document-bound changes, and notifications it passes through.

use std::ops::Range;

use scrive_core::intel::inlay;
use scrive_core::{
    CompletionItem, Diagnostic, DocId, EditOp, HoverInfo, Revision, SignatureInfo, Ticket,
};

use crate::{edits, message, uri, Encoding};

/// One thing a host must act on.
#[derive(Clone, Debug)]
pub enum Update {
    /// A change bound for one open document.
    Document(Document),
    /// A server notification the client does not consume (`window/logMessage`, `$/progress`, …).
    Notification(message::Notification),
    /// A rename's edits for a file that is not open, for the host to apply on disk.
    FileEdits(FileEdits),
}

/// A change for one document, stamped with what it is valid against.
#[derive(Clone, Debug)]
pub struct Document {
    doc_id: DocId,
    stamp: Stamp,
    change: Change,
}

/// What a [`Document`] update is valid against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stamp {
    /// The answer to the editor request that carried this ticket.
    Ticket(Ticket),
    /// Computed against this document revision (diagnostics, workspace edits).
    Revision(Revision),
}

/// The change itself.
#[derive(Clone, Debug)]
pub enum Change {
    /// The document's full diagnostic set, replacing the previous one.
    Diagnostics(Vec<Diagnostic>),
    /// Completion items answering the ticket's request; an empty list closes the popup.
    Completions(Vec<CompletionItem>),
    /// The signature answering the ticket's request; `None` closes the box.
    Signature(Option<SignatureInfo>),
    /// The hover card answering the ticket's request; `None` means no docs.
    Hover(Option<HoverInfo>),
    /// A batch of edits for one `edit_grouped` transaction: LF text, each op trimmed to what
    /// changes, sorted by `(start, end)` with tied inserts in the server's order.
    Edits(Vec<EditOp>),
    /// Where the definition under the requested offset lives; `None` when there is none, or it
    /// is in another open document that moved since the request.
    Definition(Option<Target>),
    /// The inlay hints answering the ticket's request, replacing the shown set; an empty list
    /// clears it. `None` means the fetch failed: the editor keeps what it shows.
    Inlays(Option<Vec<inlay::Placed>>),
}

/// Where a definition lives, relative to the requesting document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// In the requesting document: the byte span to select.
    Local(Range<u32>),
    /// In another document open on this client.
    Open(jump::Open),
    /// In a document this client has not opened.
    Unopened(jump::Unopened),
}

/// A definition outside the requesting editor, for the host to route: [`Open`](Jump::Open) goes
/// to the editor holding that document, and [`Unopened`](Jump::Unopened) needs the file read
/// first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Jump {
    /// In another document open on this client.
    Open(jump::Open),
    /// In a document this client has not opened.
    Unopened(jump::Unopened),
}

/// Why an editor did not apply an update. Nothing was changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// The update is for another document.
    #[error("the update is for another document")]
    Foreign,
    /// The update was computed for text the editor has moved past, or answers a request the
    /// editor no longer awaits.
    #[error("the update is stale")]
    Stale,
    /// The edit batch was rejected: overlapping ranges, or growth past the `u32` offset space.
    #[error("the edit batch was rejected")]
    Overlap,
}

/// What an editor did with one [`Document`] update: the host sends `messages`, routes `jump`,
/// and may log `refused`.
#[must_use = "Applied.messages must reach the server, and Applied.jump must be routed"]
#[derive(Debug, Default)]
pub struct Applied {
    /// Messages to send, from the sync that follows every update.
    pub messages: Vec<message::Message>,
    /// A definition in another document. Present only when the editor still awaited it.
    pub jump: Option<Jump>,
    /// Why the update was not applied, if it was not.
    pub refused: Option<Refusal>,
}

/// A rename's edits for a file that is not open: the host reads the file, calls
/// [`apply`](FileEdits::apply), and writes the result back.
#[derive(Clone, Debug)]
pub struct FileEdits {
    uri: uri::Key,
    edits: Vec<lsp_types::TextEdit>,
    encoding: Encoding,
}

impl FileEdits {
    pub(crate) fn new(uri: uri::Key, edits: Vec<lsp_types::TextEdit>, encoding: Encoding) -> Self {
        Self {
            uri,
            edits,
            encoding,
        }
    }

    /// The file to edit.
    #[must_use]
    pub fn uri(&self) -> &uri::Key {
        &self.uri
    }

    /// `text`, the file's content, with the edits applied. Text containing `\r\n` comes back
    /// with `\r\n` line breaks, and any other text with `\n`. Of two overlapping edits, which
    /// the protocol forbids, the one that sorts first by `(start, end)` applies and the other is
    /// skipped.
    #[must_use]
    pub fn apply(&self, text: &str) -> String {
        let crlf = text.contains("\r\n");
        let mut out = edits::lf(text);
        let ops = edits::hygiene(edits::Text::Str(&out), self.encoding, &self.edits);
        let mut end = 0;
        let kept: Vec<EditOp> = ops
            .into_iter()
            .filter(|op| {
                let disjoint = op.range.start >= end;
                if disjoint {
                    end = op.range.end;
                }
                disjoint
            })
            .collect();
        // Descending, so the offsets of the ops still to apply stay valid.
        for op in kept.iter().rev() {
            out.replace_range(op.range.start as usize..op.range.end as usize, &op.text);
        }
        if crlf {
            out.replace('\n', "\r\n")
        } else {
            out
        }
    }
}

/// Definition targets outside the requesting document.
pub mod jump {
    use std::ops::Range;

    use scrive_core::{DocId, Revision};

    use crate::{uri, Encoding};

    /// A definition in another open document, valid while that document is still at
    /// [`revision`](Open::revision).
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Open {
        doc_id: DocId,
        revision: Revision,
        span: Range<u32>,
    }

    impl Open {
        pub(crate) fn new(doc_id: DocId, revision: Revision, span: Range<u32>) -> Self {
            Self {
                doc_id,
                revision,
                span,
            }
        }

        /// The document the definition is in.
        #[must_use]
        pub fn doc_id(&self) -> DocId {
            self.doc_id
        }

        /// The revision of that document [`span`](Open::span) is in.
        #[must_use]
        pub fn revision(&self) -> Revision {
            self.revision
        }

        /// The byte span of the definition in that document.
        #[must_use]
        pub fn span(&self) -> Range<u32> {
            self.span.clone()
        }
    }

    /// A definition in a file this client has not opened. The host reads the file, and
    /// [`span`](Unopened::span) finds the definition in its text.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Unopened {
        uri: uri::Key,
        range: lsp_types::Range,
        encoding: Encoding,
    }

    impl Unopened {
        pub(crate) fn new(uri: uri::Key, range: lsp_types::Range, encoding: Encoding) -> Self {
            Self {
                uri,
                range,
                encoding,
            }
        }

        /// The file the definition is in.
        #[must_use]
        pub fn uri(&self) -> &uri::Key {
            &self.uri
        }

        /// The definition's byte span in `text`, the LF text an editor holds once it loads the
        /// file. The span is clamped into `text`, so a file that changed on disk still yields a
        /// valid one.
        #[must_use]
        pub fn span(&self, text: &str) -> Range<u32> {
            let span = self.encoding.text_span(text, self.range);
            span.start as u32..span.end as u32
        }
    }
}

impl Document {
    pub(crate) fn new(doc_id: DocId, stamp: Stamp, change: Change) -> Self {
        Self {
            doc_id,
            stamp,
            change,
        }
    }

    /// The document this change is for.
    #[must_use]
    pub fn doc_id(&self) -> DocId {
        self.doc_id
    }

    /// What the change is valid against.
    #[must_use]
    pub fn stamp(&self) -> Stamp {
        self.stamp
    }

    /// The change.
    #[must_use]
    pub fn change(&self) -> &Change {
        &self.change
    }

    /// All three parts, for hosts that route changes without the `CodeEditor` glue. The bundle's
    /// guarantee ends here: checking the stamp against the document becomes the caller's job.
    #[must_use]
    pub fn into_parts(self) -> (DocId, Stamp, Change) {
        (self.doc_id, self.stamp, self.change)
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use lsp_types::Position;

    use super::*;

    fn key(text: &str) -> uri::Key {
        uri::normalize(&lsp_types::Uri::from_str(text).expect("fixture URI parses"))
    }

    fn edit(start: (u32, u32), end: (u32, u32), text: &str) -> lsp_types::TextEdit {
        lsp_types::TextEdit::new(
            lsp_types::Range::new(Position::new(start.0, start.1), Position::new(end.0, end.1)),
            text.to_owned(),
        )
    }

    /// A CRLF file stays CRLF after its edits.
    #[test]
    fn file_edits_apply_keeps_crlf_line_endings() {
        let file_edits = FileEdits::new(
            key("file:///w/c.rs"),
            vec![edit((1, 0), (1, 1), "X")],
            Encoding::Utf8,
        );
        assert_eq!(file_edits.apply("a\r\nb\r\n"), "a\r\nX\r\n", "CRLF survives");
        assert_eq!(file_edits.apply("a\nb\n"), "a\nX\n", "LF survives");
    }

    /// Of two overlapping edits the first by `(start, end)` applies.
    #[test]
    fn file_edits_apply_skips_an_overlapping_edit() {
        let file_edits = FileEdits::new(
            key("file:///w/c.rs"),
            vec![edit((0, 0), (0, 3), "x"), edit((0, 1), (0, 4), "y")],
            Encoding::Utf8,
        );
        assert_eq!(file_edits.apply("abcd"), "xd", "the second edit is skipped");
    }

    /// The server's range converts against the text the host hands in, in the negotiated unit.
    #[test]
    fn unopened_span_converts_against_the_given_text() {
        let range = lsp_types::Range::new(Position::new(1, 1), Position::new(1, 2));
        let unopened = jump::Unopened::new(key("file:///w/c.rs"), range, Encoding::Utf16);
        assert_eq!(unopened.span("é\nab"), 4..5, "line 1 starts after the two-byte é");
    }
}

//! What the client hands back: document-bound changes, and notifications it passes through.

use std::ops::Range;

use scrive_core::{
    CompletionItem, Diagnostic, DocId, EditOp, HoverInfo, Revision, SignatureInfo, Ticket,
};

use crate::message;

/// One thing a host must act on.
#[derive(Clone, Debug)]
pub enum Update {
    /// A change bound for one open document.
    Document(Document),
    /// A server notification the client does not consume (`window/logMessage`, `$/progress`, …).
    Notification(message::Notification),
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
    use crate::{uri, Encoding};

    /// The server's range converts against the text the host hands in, in the negotiated unit.
    #[test]
    fn unopened_span_converts_against_the_given_text() {
        let key = uri::normalize(&lsp_types::Uri::from_str("file:///w/c.rs").expect("parses"));
        let range = lsp_types::Range::new(Position::new(1, 1), Position::new(1, 2));
        let unopened = jump::Unopened::new(key, range, Encoding::Utf16);
        assert_eq!(unopened.span("é\nab"), 4..5, "line 1 starts after the two-byte é");
    }
}

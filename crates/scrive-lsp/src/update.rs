//! What the client hands back: document-bound changes, and notifications it passes through.

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

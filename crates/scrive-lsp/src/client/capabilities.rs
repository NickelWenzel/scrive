//! What the client advertises, and the subset of the server's capabilities it acts on.

use lsp_types::{
    ClientCapabilities, CompletionClientCapabilities, CompletionItemCapability,
    GeneralClientCapabilities, MarkupKind, PublishDiagnosticsClientCapabilities,
    ServerCapabilities, TextDocumentClientCapabilities, TextDocumentSyncCapability,
    TextDocumentSyncClientCapabilities, TextDocumentSyncKind, WorkspaceClientCapabilities,
};

use crate::Encoding;

/// The server's capabilities, reduced to what the client acts on.
#[derive(Debug)]
pub(crate) struct Server {
    /// Whether the server wants `didOpen` and `didClose`.
    pub(crate) open_close: bool,
    /// How `didChange` is sent; anything but FULL or INCREMENTAL means not at all.
    pub(crate) change: TextDocumentSyncKind,
    /// The completion trigger strings; `None` when the server has no completion provider.
    pub(crate) completion: Option<Vec<String>>,
}

impl Server {
    /// An absent `textDocumentSync`, and an options object without `openClose` or `change`,
    /// mean the spec's defaults: no open/close notifications and no changes.
    pub(crate) fn new(capabilities: &ServerCapabilities) -> Self {
        let (open_close, change) = match &capabilities.text_document_sync {
            // LSP §textDocument_synchronization: a bare kind is the shorthand for
            // `{ openClose: true, change: kind }`.
            Some(TextDocumentSyncCapability::Kind(kind)) => (true, *kind),
            Some(TextDocumentSyncCapability::Options(options)) => (
                options.open_close.unwrap_or(false),
                options.change.unwrap_or(TextDocumentSyncKind::NONE),
            ),
            None => (false, TextDocumentSyncKind::NONE),
        };
        let completion = capabilities
            .completion_provider
            .as_ref()
            .map(|options| options.trigger_characters.clone().unwrap_or_default());
        Self {
            open_close,
            change,
            completion,
        }
    }
}

/// The capabilities sent in `initialize`.
pub(crate) fn client() -> ClientCapabilities {
    ClientCapabilities {
        workspace: Some(WorkspaceClientCapabilities {
            configuration: Some(true),
            workspace_folders: Some(true),
            ..WorkspaceClientCapabilities::default()
        }),
        text_document: Some(TextDocumentClientCapabilities {
            synchronization: Some(TextDocumentSyncClientCapabilities::default()),
            publish_diagnostics: Some(PublishDiagnosticsClientCapabilities {
                version_support: Some(true),
                ..PublishDiagnosticsClientCapabilities::default()
            }),
            completion: Some(CompletionClientCapabilities {
                completion_item: Some(CompletionItemCapability {
                    snippet_support: Some(true),
                    // The popup renders documentation as plain text; markdown from a server that
                    // ignores this is lowered.
                    documentation_format: Some(vec![MarkupKind::PlainText]),
                    ..CompletionItemCapability::default()
                }),
                context_support: Some(true),
                ..CompletionClientCapabilities::default()
            }),
            ..TextDocumentClientCapabilities::default()
        }),
        general: Some(GeneralClientCapabilities {
            // Preference order: utf-8 positions need no conversion walk.
            position_encodings: Some(
                [Encoding::Utf8, Encoding::Utf16, Encoding::Utf32]
                    .map(Encoding::kind)
                    .to_vec(),
            ),
            ..GeneralClientCapabilities::default()
        }),
        ..ClientCapabilities::default()
    }
}

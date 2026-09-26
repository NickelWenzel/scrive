//! What the client advertises, and the subset of the server's capabilities it acts on.

use lsp_types::{
    ClientCapabilities, GeneralClientCapabilities, PublishDiagnosticsClientCapabilities,
    ServerCapabilities, TextDocumentClientCapabilities, TextDocumentSyncCapability,
    TextDocumentSyncClientCapabilities, WorkspaceClientCapabilities,
};

use crate::Encoding;

/// The server's capabilities, reduced to what the client acts on.
#[derive(Debug)]
pub(crate) struct Server {
    /// Whether the server wants `didOpen` and `didClose`.
    pub(crate) open_close: bool,
}

impl Server {
    pub(crate) fn new(capabilities: &ServerCapabilities) -> Self {
        let open_close = match &capabilities.text_document_sync {
            // LSP §textDocument_synchronization: a bare kind is the shorthand for
            // `{ openClose: true, change: kind }`.
            Some(TextDocumentSyncCapability::Kind(_)) => true,
            Some(TextDocumentSyncCapability::Options(options)) => {
                options.open_close.unwrap_or(false)
            }
            None => false,
        };
        Self { open_close }
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

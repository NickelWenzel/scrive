//! What a server logged or asked the user to see.

/// Where a log entry came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The server, through `window/logMessage` or `window/showMessage`.
    Server,
}

/// One line a server logged or asked to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    source: Source,
    level: lsp_types::MessageType,
    shown: bool,
    text: String,
}

impl Entry {
    /// A `window/logMessage`.
    pub(crate) fn logged(params: lsp_types::LogMessageParams) -> Self {
        Self {
            source: Source::Server,
            level: params.typ,
            shown: false,
            text: params.message,
        }
    }

    /// A `window/showMessage`.
    pub(crate) fn shown(params: lsp_types::ShowMessageParams) -> Self {
        Self {
            source: Source::Server,
            level: params.typ,
            shown: true,
            text: params.message,
        }
    }

    /// Where the entry came from.
    #[must_use]
    pub fn source(&self) -> Source {
        self.source
    }

    /// How severe it is: error, warning, info or log.
    #[must_use]
    pub fn level(&self) -> lsp_types::MessageType {
        self.level
    }

    /// Whether the server asked for it to be shown to the user (`window/showMessage`), as a
    /// popup or a status line, rather than only logged.
    #[must_use]
    pub fn is_shown(&self) -> bool {
        self.shown
    }

    /// The text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

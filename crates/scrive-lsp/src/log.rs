//! What a server logged or asked the user to see.

/// Where a log entry came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The server, through `window/logMessage` or `window/showMessage`.
    Server,
    /// A line the server process wrote to its stderr.
    Stderr,
    /// A line the server process wrote to its stdout between protocol messages.
    Stdout,
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

    /// A line from the server process's stderr.
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn stderr(text: String) -> Self {
        Self::output(Source::Stderr, text)
    }

    /// A line of non-LSP output from the server process's stdout.
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn stdout(text: String) -> Self {
        Self::output(Source::Stdout, text)
    }

    #[cfg(not(target_family = "wasm"))]
    fn output(source: Source, text: String) -> Self {
        Self {
            source,
            level: lsp_types::MessageType::LOG,
            shown: false,
            text,
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

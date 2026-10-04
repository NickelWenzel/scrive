//! `tree_sitter` — the `minimal` integration with a tree-sitter grammar in place
//! of a `.sublime-syntax` one.
//!
//! ```text
//! cargo run -p scrive-iced --example tree_sitter --features tree-sitter
//! ```
//!
//! The grammar is a grammar crate's `LANGUAGE` plus its highlights query; the
//! bundled Scrive Dark theme colors tree-sitter captures as well as syntect
//! scopes. For wasm32-unknown-unknown, run `eval "$(scripts/wasm-cflags.sh)"`
//! first: tree-sitter-rust's C needs the wasm libc headers.

use iced::time::Instant;
use iced::{Element, Subscription, Task};

use scrive_core::TreeSitterDef;
use scrive_iced::{CodeEditor, Event};

/// The whole application state: the editor owns its document.
struct App {
    editor: CodeEditor,
}

/// The host message type — it only ever *maps* the editor's opaque [`Event`]; it
/// never matches on it.
#[derive(Debug, Clone)]
enum Message {
    Editor(Event),
}

impl App {
    fn new() -> Self {
        let grammar = TreeSitterDef::new(tree_sitter_rust::LANGUAGE, tree_sitter_rust::HIGHLIGHTS_QUERY)
            .expect("tree-sitter-rust's own highlights query compiles");
        let source = "fn main() {\n    // edit me — tree-sitter reparses as you type\n    println!(\"hello, scrive\");\n}\n";
        Self { editor: CodeEditor::new(source).language(grammar) }
    }

    /// `now` is the instant iced stamps on the message (see `main`).
    fn update(&mut self, message: Message, now: Instant) -> Task<Message> {
        match message {
            Message::Editor(event) => self.editor.update(event, now).map(Message::Editor),
        }
    }

    fn view(&self) -> Element<'_, Message> {
        self.editor.view().map(Message::Editor)
    }

    fn subscription(&self) -> Subscription<Message> {
        self.editor.subscription().map(Message::Editor)
    }
}

fn main() -> iced::Result {
    // `timed` hands `update` each message's instant, which the editor's find
    // debounce runs on.
    let app = iced::application::timed(App::new, App::update, App::subscription, App::view)
        .title("scrive — tree-sitter");
    // Register the fonts the widget needs (fold chevrons + find-bar icons).
    app.fonts(scrive_iced::required_fonts().iter().copied()).run()
}

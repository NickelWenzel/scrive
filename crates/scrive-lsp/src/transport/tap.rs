//! Stand-ins for a writer thread and a worker, for tests that read what the client sends.

use std::sync::mpsc;

use super::writer::{Item, Outgoing};
use super::{Generation, Handle, Lifecycle, Link, Notice, Writer};

/// A stream link's queue, read by the test instead of a writer thread.
pub(crate) struct Queue(Outgoing);

/// A worker's inbox, read by the test.
pub(crate) struct Inbox(mpsc::Receiver<Notice>);

impl Queue {
    /// A link of connection `generation`, and the tap on its queue. Its guard never trips.
    pub(crate) fn link(generation: Generation) -> (Link, Self) {
        let (notices, _) = mpsc::channel();
        let (writer, outgoing) = Writer::new(generation, usize::MAX, &notices);
        (Link::Stream(writer), Self(outgoing))
    }

    /// What was queued since the last call: each body as JSON, `None` for the close.
    pub(crate) fn items(&self) -> Vec<Option<serde_json::Value>> {
        self.0
            .queued()
            .into_iter()
            .map(|item| match item {
                Item::Body(body) => {
                    Some(serde_json::from_slice(&body).expect("queued bodies are JSON"))
                }
                Item::Close => None,
            })
            .collect()
    }
}

impl Inbox {
    /// A control line to nobody but the test.
    pub(crate) fn control() -> (Handle, Self) {
        let (notices, inbox) = mpsc::channel();
        (Handle { notices }, Self(inbox))
    }

    /// What the client told the worker since the last call.
    pub(crate) fn lifecycles(&self) -> Vec<Lifecycle> {
        self.0
            .try_iter()
            .filter_map(|notice| match notice {
                Notice::Control(lifecycle) => Some(lifecycle),
                Notice::Ended { .. }
                | Notice::WriteFailed(_)
                | Notice::Backlog(_)
                | Notice::Replied(_) => None,
            })
            .collect()
    }
}

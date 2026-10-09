//! The reader of a connection's LSP byte stream: frames become messages, the text between them
//! becomes log lines.

use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

use super::{frame, lifecycle, Feed, Generation, Notice, Pipe};
use crate::{client, log, transport};

/// Bytes per read.
pub(crate) const CHUNK: usize = 64 * 1024;

/// Where one connection's stream reader sends what it decodes.
pub(crate) struct Reader {
    pub(crate) events: Feed,
    pub(crate) notices: mpsc::Sender<Notice>,
    pub(crate) generation: Generation,
    /// The stream it reads, which names its noise and its end.
    pub(crate) pipe: Pipe,
    /// Tells the reader to look for the shutdown reply.
    pub(crate) closing: Arc<AtomicBool>,
}

impl Reader {
    /// Decodes `stream` until EOF, then tells the worker. It reads on after `Events` is dropped,
    /// so the server never blocks on a full pipe. Once `closing` is set, the reply to the
    /// shutdown request goes to the worker instead of the client.
    pub(crate) fn read(self, mut stream: impl Read) {
        let mut decoder = frame::Decoder::default();
        let mut chunk = vec![0; CHUNK];
        loop {
            let read = match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            if !self.deliver(decoder.push(&chunk[..read])) {
                break;
            }
        }
        if let Some(line) = decoder.finish() {
            let entry = self.pipe.noise(line);
            let _ = self
                .events
                .unbounded_send(transport::Event::Log(Arc::from([entry])));
        }
        let _ = self.notices.send(Notice::Ended {
            generation: self.generation,
            pipe: self.pipe,
        });
    }

    /// Forwards one read's items in order, consecutive noise lines as one log batch. `false`
    /// once the stream is corrupt.
    fn deliver(&self, items: Vec<frame::Item>) -> bool {
        let generation = self.generation;
        let mut noise = Vec::new();
        for item in items {
            let (event, corrupt) = match item {
                frame::Item::Noise(text) => {
                    noise.push(self.pipe.noise(text));
                    continue;
                }
                frame::Item::Body(body)
                    if self.closing.load(Ordering::Relaxed)
                        && lifecycle::is_shutdown_reply(&body) =>
                {
                    let _ = self.notices.send(Notice::Replied(generation));
                    continue;
                }
                frame::Item::Body(body) => (
                    transport::Event::Message {
                        generation,
                        body: Arc::from(body),
                    },
                    false,
                ),
                frame::Item::Skipped { length } => (self.oversized(length), false),
                frame::Item::Corrupt { length } => (self.oversized(length), true),
            };
            flush(&mut noise, &self.events);
            let _ = self.events.unbounded_send(event);
            if corrupt {
                return false;
            }
        }
        flush(&mut noise, &self.events);
        true
    }

    fn oversized(&self, length: u64) -> transport::Event {
        transport::Event::Error {
            generation: self.generation,
            error: client::Error::Oversized { length },
        }
    }
}

/// Sends the batched noise lines as one log event, if there are any.
fn flush(noise: &mut Vec<log::Entry>, events: &Feed) {
    if !noise.is_empty() {
        let _ = events.unbounded_send(transport::Event::Log(Arc::from(std::mem::take(noise))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything the reader sent, in order.
    fn sent(
        mut incoming: futures_channel::mpsc::UnboundedReceiver<transport::Event>,
    ) -> Vec<transport::Event> {
        let mut events = Vec::new();
        while let Ok(event) = incoming.try_recv() {
            events.push(event);
        }
        events
    }

    /// The texts of a log event's entries, or `None` for any other event.
    fn texts(event: &transport::Event) -> Option<Vec<&str>> {
        let transport::Event::Log(entries) = event else {
            return None;
        };
        Some(entries.iter().map(log::Entry::text).collect())
    }

    fn body(event: &transport::Event) -> Option<&[u8]> {
        let transport::Event::Message { body, .. } = event else {
            return None;
        };
        Some(body)
    }

    /// Reads `stream` as stdout of connection 0, with the shutdown reply looked for if
    /// `closing`; returns what the client and the worker were sent.
    fn read(stream: &[u8], closing: bool) -> (Vec<transport::Event>, Vec<Notice>) {
        let (events, incoming) = futures_channel::mpsc::unbounded();
        let (notices, received) = mpsc::channel();
        Reader {
            events,
            notices,
            generation: Generation::FIRST,
            pipe: Pipe::Stdout,
            closing: Arc::new(AtomicBool::new(closing)),
        }
        .read(stream);
        (sent(incoming), received.try_iter().collect())
    }

    /// Frames become messages, and the noise lines between them one log batch per run.
    #[test]
    fn the_reader_forwards_frames_and_batches_noise_per_read() {
        let mut stdout = b"banner\n".to_vec();
        stdout.extend(frame::encode(b"{\"a\":1}"));
        stdout.extend(b"a\nb\n");
        stdout.extend(frame::encode(b"{\"b\":2}"));
        let (events, notices) = read(&stdout, false);
        assert_eq!(events.len(), 4, "four events: {events:?}");
        assert_eq!(texts(&events[0]), Some(vec!["banner"]), "the banner");
        assert_eq!(body(&events[1]), Some(&b"{\"a\":1}"[..]), "the first frame");
        assert_eq!(
            texts(&events[2]),
            Some(vec!["a", "b"]),
            "both lines in one batch"
        );
        assert_eq!(
            body(&events[3]),
            Some(&b"{\"b\":2}"[..]),
            "the second frame"
        );
        assert!(
            matches!(
                notices.as_slice(),
                [Notice::Ended {
                    pipe: Pipe::Stdout,
                    ..
                }]
            ),
            "the worker hears of the EOF"
        );
        assert!(
            matches!(&events[0], transport::Event::Log(entries)
                if entries[0].source() == log::Source::Stdout),
            "the noise is stdout's"
        );
    }

    /// While closing, the reply to the shutdown request goes to the worker, not the client;
    /// before that it is an ordinary message.
    #[test]
    fn the_reader_swallows_the_shutdown_reply_only_while_closing() {
        let reply = frame::encode(br#"{"id":"scrive-lsp/shutdown","result":null}"#);
        for (closing, forwarded) in [(false, 1), (true, 0)] {
            let (events, notices) = read(&reply, closing);
            assert_eq!(events.len(), forwarded, "closing: {closing}");
            let replied = notices
                .iter()
                .any(|notice| matches!(notice, Notice::Replied(_)));
            assert_eq!(replied, closing, "the worker hears of it while closing");
        }
    }

    /// A length above 1 GiB is reported and ends the reading; a frame after it is not
    /// forwarded.
    #[test]
    fn the_reader_stops_on_a_corrupt_length() {
        let mut stdout = b"Content-Length: 2000000000\r\n\r\n".to_vec();
        stdout.extend(frame::encode(b"{}"));
        let (events, notices) = read(&stdout, false);
        assert!(
            matches!(
                events.as_slice(),
                [transport::Event::Error {
                    error: client::Error::Oversized {
                        length: 2_000_000_000
                    },
                    ..
                }]
            ),
            "only the error: {events:?}"
        );
        assert!(
            matches!(
                notices.as_slice(),
                [Notice::Ended {
                    pipe: Pipe::Stdout,
                    ..
                }]
            ),
            "the worker hears of the end"
        );
    }
}

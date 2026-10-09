//! One connection's outgoing queue for the bridges whose worker writes a byte stream, and the
//! hung-server guard on it.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};

use super::{frame, Generation, Notice};

/// One connection's outgoing queue. Bodies go to the writer thread, which frames and writes them
/// in order. Clones share the queue; the server's input closes on [`Writer::close`], or once
/// every clone is gone.
#[derive(Clone, Debug)]
pub(crate) struct Writer {
    queue: mpsc::Sender<Item>,
    backlog: Arc<Backlog>,
    /// Where the guard reports its trip.
    notices: mpsc::Sender<Notice>,
    generation: Generation,
}

/// The writer thread's end of a connection's queue.
#[derive(Debug)]
pub(crate) struct Outgoing {
    items: mpsc::Receiver<Item>,
    backlog: Arc<Backlog>,
    notices: mpsc::Sender<Notice>,
    generation: Generation,
}

/// One entry of a connection's outgoing queue.
#[derive(Debug)]
pub(crate) enum Item {
    /// A serialized message, framed by the writer thread.
    Body(Arc<[u8]>),
    /// Close the server's input; nothing after it is written.
    Close,
}

/// Bytes queued for the server but not yet written, and the hung-server guard on them.
#[derive(Debug)]
struct Backlog {
    unwritten: AtomicUsize,
    limit: usize,
    tripped: AtomicBool,
}

impl Writer {
    /// The queue of connection `generation`, whose guard trips past `limit` unwritten bytes and
    /// tells `notices`.
    pub(crate) fn new(
        generation: Generation,
        limit: usize,
        notices: &mpsc::Sender<Notice>,
    ) -> (Self, Outgoing) {
        let (queue, items) = mpsc::channel();
        let backlog = Arc::new(Backlog {
            unwritten: AtomicUsize::new(0),
            limit,
            tripped: AtomicBool::new(false),
        });
        let writer = Self {
            queue,
            backlog: Arc::clone(&backlog),
            notices: notices.clone(),
            generation,
        };
        let outgoing = Outgoing {
            items,
            backlog,
            notices: notices.clone(),
            generation,
        };
        (writer, outgoing)
    }

    /// Queues one serialized message. Dropped once the guard has tripped or the writer is gone.
    pub(crate) fn send(&self, body: Arc<[u8]>) {
        if self.backlog.tripped.load(Ordering::Relaxed) {
            return;
        }
        let length = body.len();
        // Counted before the send and given back after the write, so the count never goes
        // below zero.
        let unwritten = self.backlog.unwritten.fetch_add(length, Ordering::Relaxed) + length;
        if self.queue.send(Item::Body(body)).is_err() {
            self.backlog.unwritten.fetch_sub(length, Ordering::Relaxed);
            return;
        }
        // Checked here, on the sending thread: the writer thread is the one stuck in
        // `write_all` when the server stops reading.
        if unwritten > self.backlog.limit && !self.backlog.tripped.swap(true, Ordering::Relaxed) {
            let _ = self.notices.send(Notice::Backlog(self.generation));
        }
    }

    /// Queues the close of the server's input behind everything queued so far.
    pub(crate) fn close(&self) {
        let _ = self.queue.send(Item::Close);
    }
}

impl Outgoing {
    /// Frames and writes every queued body to `sink`, each in one write, until the close or until
    /// every [`Writer`] is gone, then hands `sink` to `close`. A failed write is reported to the
    /// worker instead, and `sink` is dropped.
    pub(crate) fn write<W: Write>(self, mut sink: W, close: impl FnOnce(W)) {
        for item in &self.items {
            let Item::Body(body) = item else {
                break;
            };
            let written = sink.write_all(&frame::encode(&body));
            self.backlog
                .unwritten
                .fetch_sub(body.len(), Ordering::Relaxed);
            if written.is_err() {
                let _ = self.notices.send(Notice::WriteFailed(self.generation));
                return;
            }
        }
        close(sink);
    }

    /// What was queued since the last call, without writing it.
    #[cfg(test)]
    pub(crate) fn queued(&self) -> Vec<Item> {
        self.items.try_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A writer whose queue nobody drains, with room for `limit` bytes.
    fn stalled(limit: usize) -> (Writer, Outgoing, mpsc::Receiver<Notice>) {
        let (notices, received) = mpsc::channel();
        let (writer, outgoing) = Writer::new(Generation::FIRST, limit, &notices);
        (writer, outgoing, received)
    }

    /// The first send past the limit trips the guard once; later sends are dropped.
    #[test]
    fn the_guard_trips_once_past_its_limit() {
        let (writer, outgoing, received) = stalled(10);
        writer.send(Arc::from(&b"123456"[..]));
        assert!(
            received.try_recv().is_err(),
            "below the limit nothing trips"
        );
        writer.send(Arc::from(&b"123456"[..]));
        assert!(
            matches!(received.try_recv(), Ok(Notice::Backlog(Generation::FIRST))),
            "past the limit the guard trips"
        );
        writer.send(Arc::from(&b"123456"[..]));
        assert!(received.try_recv().is_err(), "it trips once");
        assert_eq!(
            outgoing.queued().len(),
            2,
            "the send after the trip is dropped"
        );
    }

    /// A send that finds the writer gone takes its bytes back, so a dead writer never trips the
    /// guard.
    #[test]
    fn a_send_to_a_gone_writer_gives_its_bytes_back() {
        let (writer, outgoing, received) = stalled(10);
        drop(outgoing);
        for _ in 0..3 {
            writer.send(Arc::from(&b"123456"[..]));
        }
        assert_eq!(
            writer.backlog.unwritten.load(Ordering::Relaxed),
            0,
            "nothing is counted"
        );
        assert!(received.try_recv().is_err(), "the guard never trips");
    }

    /// Each body leaves framed, in order; the close hands the sink back and ends the writing.
    #[test]
    fn the_writer_frames_each_body_and_stops_at_the_close() {
        let (notices, _received) = mpsc::channel();
        let (writer, outgoing) = Writer::new(Generation::FIRST, usize::MAX, &notices);
        writer.send(Arc::from(&b"{}"[..]));
        writer.close();
        writer.send(Arc::from(&b"[]"[..]));
        let mut closed = None;
        outgoing.write(Vec::new(), |sink| closed = Some(sink));
        assert_eq!(
            closed.as_deref(),
            Some(frame::encode(b"{}").as_slice()),
            "one frame, then the close"
        );
    }
}

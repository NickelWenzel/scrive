//! The in-process bridge: the client end of `lsp_server::Connection::memory()`.

#[cfg(not(target_family = "wasm"))]
mod watcher;

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
use core::future::Future;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
use core::pin::Pin;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
use core::time::Duration;
use std::sync::Arc;
use std::task::{Context, Poll};

use crate::{client, transport};

/// How often the browser polls the channel, which has no waker.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
const POLL: Duration = Duration::from_millis(10);

/// The client's sending half. It parses each serialized message into lsp-server's type, since
/// the server end speaks `lsp_server::Message`: one extra parse on the calling thread, the price
/// of an in-process server.
#[derive(Clone, Debug)]
pub(crate) struct Link(crossbeam_channel::Sender<lsp_server::Message>);

/// The client's receiving half.
pub(crate) struct Inbox {
    receiver: crossbeam_channel::Receiver<lsp_server::Message>,
    #[cfg(not(target_family = "wasm"))]
    watcher: watcher::Watcher,
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    tick: Option<Pin<Box<wasmtimer::tokio::Sleep>>>,
}

impl Link {
    pub(crate) fn new(sender: crossbeam_channel::Sender<lsp_server::Message>) -> Self {
        Self(sender)
    }

    pub(crate) fn send(&self, body: &[u8]) {
        // An answer echoes the server's own request id, which scrive's envelope accepts in forms
        // lsp-server rejects (a float, `null`); such an answer is dropped, not a panic.
        let Ok(message) = serde_json::from_slice::<lsp_server::Message>(body) else {
            return;
        };
        // A server end that is gone shows up as the inbox's disconnect.
        let _ = self.0.send(message);
    }
}

impl Inbox {
    pub(crate) fn new(receiver: crossbeam_channel::Receiver<lsp_server::Message>) -> Self {
        Self {
            receiver,
            #[cfg(not(target_family = "wasm"))]
            watcher: watcher::Watcher::Idle,
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            tick: None,
        }
    }

    pub(crate) fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<transport::Event> {
        if let Some(event) = self.try_next() {
            return Poll::Ready(event);
        }
        self.wait(cx)
    }

    /// The next message, or the disconnect, without waiting.
    fn try_next(&self) -> Option<transport::Event> {
        match self.receiver.try_recv() {
            Ok(message) => Some(transport::Event::Message {
                generation: transport::Generation::FIRST,
                body: encode(&message),
            }),
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                Some(transport::Event::Stopped(client::Reason::Closed))
            }
            Err(crossbeam_channel::TryRecvError::Empty) => None,
        }
    }

    /// Arms the watcher, then looks once more, since a message that landed before the waker was
    /// stored wakes nobody.
    #[cfg(not(target_family = "wasm"))]
    fn wait(&mut self, cx: &mut Context<'_>) -> Poll<transport::Event> {
        if let Err(error) = self.watcher.arm(&self.receiver, cx.waker()) {
            return Poll::Ready(transport::Event::Stopped(client::Reason::Failed(Arc::new(
                error,
            ))));
        }
        self.try_next().map_or(Poll::Pending, Poll::Ready)
    }

    /// Crossbeam has no waker, and an in-process server runs on this thread anyway, so the
    /// channel is polled on a timer. One `Sleep` is reused.
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    fn wait(&mut self, cx: &mut Context<'_>) -> Poll<transport::Event> {
        loop {
            let tick = self
                .tick
                .get_or_insert_with(|| Box::pin(wasmtimer::tokio::sleep(POLL)));
            if tick.as_mut().poll(cx).is_pending() {
                return Poll::Pending;
            }
            tick.as_mut().reset(wasmtimer::std::Instant::now() + POLL);
            if let Some(event) = self.try_next() {
                return Poll::Ready(event);
            }
        }
    }

    /// No waker on other wasm hosts: only a host that polls again sees new messages.
    #[cfg(all(
        target_family = "wasm",
        not(all(target_arch = "wasm32", target_os = "unknown"))
    ))]
    fn wait(&mut self, _cx: &mut Context<'_>) -> Poll<transport::Event> {
        Poll::Pending
    }
}

/// `message` as JSON text. lsp-server writes no `jsonrpc` member, which the envelope tolerates.
fn encode(message: &lsp_server::Message) -> Arc<[u8]> {
    serde_json::to_vec(message)
        .expect("lsp-server messages serialize")
        .into()
}

#[cfg(test)]
mod tests {
    use futures::future::poll_fn;
    use futures::FutureExt;

    use super::*;

    fn notification(n: u32) -> lsp_server::Message {
        lsp_server::Message::Notification(lsp_server::Notification::new("n".to_owned(), n))
    }

    /// The `n` param of a notification the inbox handed over.
    fn number(event: &transport::Event) -> u64 {
        let transport::Event::Message { body, .. } = event else {
            panic!("expected a message, got {event:?}")
        };
        let value: serde_json::Value = serde_json::from_slice(body).expect("the body is JSON");
        value["params"].as_u64().expect("the param is a number")
    }

    /// Messages the server already sent are drained in order without a waker, and an empty
    /// channel is pending.
    #[test]
    fn a_stepped_server_drains_deterministically() {
        let (near, far) = lsp_server::Connection::memory();
        let mut inbox = Inbox::new(near.receiver);
        for n in 0..3 {
            far.sender.send(notification(n)).expect("the inbox is open");
        }
        let drained: Vec<u64> = (0..3)
            .map(|_| {
                let event = poll_fn(|cx| inbox.poll_next(cx))
                    .now_or_never()
                    .expect("a sent message is ready");
                number(&event)
            })
            .collect();
        assert_eq!(drained, [0, 1, 2], "the messages arrive in order");
        assert!(
            poll_fn(|cx| inbox.poll_next(cx)).now_or_never().is_none(),
            "an empty channel is pending"
        );
    }

    #[cfg(not(target_family = "wasm"))]
    mod native {
        use std::sync::mpsc;
        use std::thread;
        use std::time::{Duration, Instant};

        use super::*;

        /// Runs `test` on its own thread and fails if it takes longer than 10 s.
        fn within_deadline<T: Send + 'static>(test: impl FnOnce() -> T + Send + 'static) -> T {
            let (done, result) = mpsc::channel();
            thread::spawn(move || {
                let _ = done.send(test());
            });
            result
                .recv_timeout(Duration::from_secs(10))
                .expect("the test finishes within its deadline")
        }

        /// Polls `condition` every millisecond for up to a second.
        fn eventually(condition: impl Fn() -> bool) -> bool {
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(1) {
                if condition() {
                    return true;
                }
                thread::sleep(Duration::from_millis(1));
            }
            condition()
        }

        /// Every message a server thread sends at uneven pace arrives once, in order.
        #[test]
        fn messages_arrive_once_and_in_order_under_block_on() {
            let received = within_deadline(|| {
                let (near, far) = lsp_server::Connection::memory();
                let server = thread::spawn(move || {
                    let mut state: u32 = 0x9E37_79B9;
                    for n in 0..2000 {
                        far.sender.send(notification(n)).expect("the inbox is open");
                        state ^= state << 13;
                        state ^= state >> 17;
                        state ^= state << 5;
                        if n % 7 == 0 {
                            thread::sleep(Duration::from_micros(u64::from(state % 51)));
                        }
                    }
                });
                let mut inbox = Inbox::new(near.receiver);
                let received: Vec<u64> = (0..2000)
                    .map(|_| {
                        number(&futures::executor::block_on(poll_fn(|cx| {
                            inbox.poll_next(cx)
                        })))
                    })
                    .collect();
                server.join().expect("the server thread finishes");
                received
            });
            assert_eq!(
                received,
                (0..2000).collect::<Vec<u64>>(),
                "all of them, in order"
            );
        }

        /// Without traffic the watcher sleeps: it wakes the poller once per message, and not
        /// in between.
        #[test]
        fn the_watcher_stays_idle_without_traffic() {
            let (near, far) = lsp_server::Connection::memory();
            let mut inbox = Inbox::new(near.receiver);
            assert!(
                poll_fn(|cx| inbox.poll_next(cx)).now_or_never().is_none(),
                "nothing has arrived"
            );
            far.sender.send(notification(0)).expect("the inbox is open");
            assert!(
                eventually(|| inbox.watcher.wakeups() == 1),
                "the message wakes the poller"
            );
            let event = poll_fn(|cx| inbox.poll_next(cx)).now_or_never();
            assert_eq!(
                event.as_ref().map(number),
                Some(0),
                "the message is drained"
            );
            assert!(
                poll_fn(|cx| inbox.poll_next(cx)).now_or_never().is_none(),
                "the channel is empty again"
            );
            thread::sleep(Duration::from_millis(100));
            assert_eq!(inbox.watcher.wakeups(), 1, "an idle watcher wakes nobody");
        }

        /// Dropping the inbox ends its watcher thread.
        #[test]
        fn the_watcher_exits_when_the_inbox_drops() {
            let (near, _far) = lsp_server::Connection::memory();
            let mut inbox = Inbox::new(near.receiver);
            assert!(
                poll_fn(|cx| inbox.poll_next(cx)).now_or_never().is_none(),
                "the first empty poll starts the watcher"
            );
            let thread = inbox
                .watcher
                .into_thread()
                .expect("the watcher was started");
            assert!(
                eventually(|| thread.is_finished()),
                "the watcher thread exits"
            );
        }

        /// A server that drops its end stops the inbox with `Closed`.
        #[test]
        fn a_dropped_server_end_stops_closed() {
            let event = within_deadline(|| {
                let (near, far) = lsp_server::Connection::memory();
                let mut inbox = Inbox::new(near.receiver);
                let dropper = thread::spawn(move || {
                    thread::sleep(Duration::from_millis(10));
                    drop(far);
                });
                let event = futures::executor::block_on(poll_fn(|cx| inbox.poll_next(cx)));
                dropper.join().expect("the dropping thread finishes");
                event
            });
            assert!(
                matches!(event, transport::Event::Stopped(client::Reason::Closed)),
                "the inbox stops closed, got {event:?}"
            );
        }
    }
}

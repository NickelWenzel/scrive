//! A thread that wakes the memory bridge's poller when the server sends, since crossbeam
//! channels have no async waker.

use std::io;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Waker;
use std::thread;

/// The watcher's side of the arm protocol.
pub(super) struct Shared {
    waker: Mutex<Option<Waker>>,
    armed: AtomicBool,
    /// How often the watcher woke a poller, so a test can tell an idle watcher from a spinning
    /// one.
    #[cfg(test)]
    wakeups: AtomicUsize,
}

/// The inbox's handle on its watcher thread, which starts on the first poll that finds nothing.
pub(super) enum Watcher {
    /// Not spawned yet.
    Idle,
    /// Running; dropping `control` makes the thread exit.
    Running {
        shared: Arc<Shared>,
        control: crossbeam_channel::Sender<()>,
        #[cfg(test)]
        thread: thread::JoinHandle<()>,
    },
}

impl Watcher {
    /// Stores `waker` to be woken when `data` has a message or disconnects, spawning the thread
    /// on first use.
    ///
    /// # Errors
    /// The thread could not be spawned.
    pub(super) fn arm(
        &mut self,
        data: &crossbeam_channel::Receiver<lsp_server::Message>,
        waker: &Waker,
    ) -> io::Result<()> {
        match self {
            Watcher::Idle => {
                let (control, signals) = crossbeam_channel::unbounded();
                let shared = Arc::new(Shared {
                    waker: Mutex::new(None),
                    armed: AtomicBool::new(false),
                    #[cfg(test)]
                    wakeups: AtomicUsize::new(0),
                });
                let thread = {
                    let (data, shared) = (data.clone(), Arc::clone(&shared));
                    thread::Builder::new()
                        .name("scrive-lsp memory".to_owned())
                        .spawn(move || watch(&data, &signals, &shared))?
                };
                shared.arm(&control, waker);
                // Outside tests the thread detaches; it exits once `control` drops.
                #[cfg(not(test))]
                drop(thread);
                *self = Watcher::Running {
                    shared,
                    control,
                    #[cfg(test)]
                    thread,
                };
            }
            Watcher::Running {
                shared, control, ..
            } => shared.arm(control, waker),
        }
        Ok(())
    }

    /// How often the thread woke a poller; `0` before it runs.
    #[cfg(test)]
    pub(super) fn wakeups(&self) -> usize {
        match self {
            Watcher::Idle => 0,
            Watcher::Running { shared, .. } => shared.wakeups.load(Ordering::Relaxed),
        }
    }

    /// The thread, if it runs, with `control` dropped so that it exits.
    #[cfg(test)]
    pub(super) fn into_thread(self) -> Option<thread::JoinHandle<()>> {
        match self {
            Watcher::Idle => None,
            Watcher::Running { thread, .. } => Some(thread),
        }
    }
}

impl Shared {
    /// Stores `waker` and wakes the thread, unless it is already armed.
    fn arm(&self, control: &crossbeam_channel::Sender<()>, waker: &Waker) {
        *self
            .waker
            .lock()
            .expect("the watcher's waker lock is never poisoned") = Some(waker.clone());
        if !self.armed.swap(true, Ordering::SeqCst) {
            // The thread is gone only if it panicked, and then nothing is left to wake.
            let _ = control.send(());
        }
    }
}

/// The thread: waits, unarmed, for a poll to arm it; then waits for `data` to hold a message or
/// disconnect, wakes the poller and goes back to waiting. It returns once `control` disconnects.
fn watch(
    data: &crossbeam_channel::Receiver<lsp_server::Message>,
    control: &crossbeam_channel::Receiver<()>,
    shared: &Shared,
) {
    while control.recv().is_ok() {
        let mut select = crossbeam_channel::Select::new();
        let ready_data = select.recv(data);
        select.recv(control);
        // `ready` doesn't consume, so the inbox still receives the message itself.
        loop {
            if select.ready() == ready_data {
                break;
            }
            // A stale arm token must be consumed, or `ready` returns at once again.
            if let Err(crossbeam_channel::TryRecvError::Disconnected) = control.try_recv() {
                return;
            }
        }
        // Disarm before taking the waker: a poll that stores its waker after this point sees
        // `armed == false` and re-arms.
        shared.armed.store(false, Ordering::SeqCst);
        let waker = shared
            .waker
            .lock()
            .expect("the watcher's waker lock is never poisoned")
            .take();
        if let Some(waker) = waker {
            #[cfg(test)]
            shared.wakeups.fetch_add(1, Ordering::Relaxed);
            waker.wake();
        }
    }
}

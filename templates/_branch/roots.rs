//! The roots bridge: blocking I/O runs on OS threads, so it never stalls the tree.
//! Written by `bonsai branch --roots`, and shared by every root. No need to edit it.

use std::sync::mpsc::{self, SyncSender};
use std::thread;
use std::time::Duration;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, TrySendError};

/// Messages from the link, waiting for the tree. Holds 16, enough for a burst.
pub struct Inbox<T: 'static>(&'static Channel<CriticalSectionRawMutex, T, 16>);

impl<T> Inbox<T> {
    /// Wait for the next message from the link.
    pub async fn next(&self) -> T {
        self.0.receive().await
    }
}

/// Messages for the link. Holds 64, so a slow link doesn't hold up the tree.
pub struct Outbox<T>(SyncSender<T>);

impl<T> Outbox<T> {
    /// Queue `msg` for the link. Never waits: if 64 are already queued, `msg`
    /// is dropped and this returns `false`.
    #[allow(dead_code)] // until a root sends something
    pub fn send(&self, msg: T) -> bool {
        self.0.try_send(msg).is_ok()
    }
}

/// Start a root's two threads. `receive` runs once on its own thread: loop on
/// your link and hand each message to `deliver`. `send` is called on another
/// thread for every message the tree puts in the outbox.
pub fn bridge<I: Send + 'static, O: Send + 'static>(
    name: &str,
    receive: impl FnOnce(&dyn Fn(I)) + Send + 'static,
    mut send: impl FnMut(O) + Send + 'static,
) -> (Inbox<I>, Outbox<O>) {
    // One inbox per root, kept for the life of the program.
    let inbox: &'static Channel<CriticalSectionRawMutex, I, 16> =
        Box::leak(Box::new(Channel::new()));
    let (outbox, outgoing) = mpsc::sync_channel::<O>(64);

    // When the tree is 16 behind, wait on this thread, never on the executor.
    let deliver = move |mut msg: I| {
        while let Err(TrySendError::Full(back)) = inbox.try_send(msg) {
            msg = back;
            thread::sleep(Duration::from_millis(1));
        }
    };
    thread::Builder::new()
        .name(format!("{name}-receive"))
        .spawn(move || receive(&deliver))
        .expect("spawn a roots thread");
    thread::Builder::new()
        .name(format!("{name}-send"))
        .spawn(move || outgoing.into_iter().for_each(&mut send))
        .expect("spawn a roots thread");

    (Inbox(inbox), Outbox(outbox))
}

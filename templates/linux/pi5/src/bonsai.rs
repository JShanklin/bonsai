//! bonsai's runtime: the core loop this tree runs on. Written by `bonsai sync`,
//! which rewrites it; don't edit it.
//!
//! Every branch runs in one core loop, one event at a time. An event goes to
//! the branches wired to it, and every message they send is delivered, in
//! order, before the next event is taken. The same events in always give the
//! same messages out.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;

/// A branch: its state, and what it decides for each input.
pub trait Branch: Sized {
    /// What it receives: one variant per wire into it, plus `Tick` when it
    /// has a `rate`. Generated as `crate::wiring::<branch>::Input`.
    type Input;
    /// Where it sends: `crate::wiring::<branch>::Out`.
    type Out: Default;

    /// Setup: the branch's starting state. Runs again after `process` panics.
    fn setup() -> Self;

    /// Process: decide what to do with one input, and `out.send(..)` the
    /// results. No I/O and no waiting here, so the same inputs always give
    /// the same outputs.
    fn process(&mut self, input: Self::Input, out: &mut Self::Out);
}

/// `out.send(message)`: there's one for each message a branch is wired to send.
pub trait Sends<M> {
    fn send(&mut self, message: M);
}

/// Something from outside the core.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event {
    /// A branch's `rate` ticked: its index in the tree.
    Tick(usize),
}

/// The generated tree, `crate::wiring::Core`.
pub trait Tree {
    /// The branches with a `rate`: (index, ticks per second).
    fn rates(&self) -> Vec<(usize, f64)>;
    /// Take one event and everything it sets off.
    fn handle(&mut self, event: Event);
}

/// One branch in the core, set up again if its `process` panics.
pub struct Slot<B: Branch> {
    name: &'static str,
    branch: B,
}

impl<B: Branch> Slot<B> {
    pub fn new(name: &'static str) -> Self {
        Slot {
            name,
            branch: B::setup(),
        }
    }

    /// Process one input; what it sent, or nothing if it panicked.
    pub fn process(&mut self, input: B::Input) -> B::Out {
        let mut out = B::Out::default();
        let branch = &mut self.branch;
        if catch_unwind(AssertUnwindSafe(|| branch.process(input, &mut out))).is_err() {
            eprintln!("bonsai: {} panicked; setting it up again", self.name);
            self.branch = B::setup();
            return B::Out::default();
        }
        out
    }
}

/// Messages one event may set off before bonsai assumes branches are sending
/// to each other without end.
pub const MAX_DELIVERIES: usize = 10_000;

/// Deliver everything queued, oldest first, until nothing is left.
pub fn drain<M>(queue: &mut VecDeque<M>, mut deliver: impl FnMut(M, &mut VecDeque<M>)) {
    let mut delivered = 0;
    while let Some(message) = queue.pop_front() {
        delivered += 1;
        if delivered > MAX_DELIVERIES {
            eprintln!(
                "bonsai: one event set off over {MAX_DELIVERIES} messages; dropping the {} left",
                queue.len() + 1
            );
            queue.clear();
            return;
        }
        deliver(message, queue);
    }
}

/// Run the tree until Ctrl-C or SIGTERM.
pub async fn run<T: Tree>(mut tree: T) {
    let (events, mut inbox) = mpsc::channel::<Event>(1024);
    for (index, hz) in tree.rates() {
        let events = events.clone();
        tokio::spawn(async move {
            let mut every = tokio::time::interval(Duration::from_secs_f64(1.0 / hz));
            every.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                every.tick().await;
                if events.send(Event::Tick(index)).await.is_err() {
                    return;
                }
            }
        });
    }
    // Made once: a signal that lands while an event is handled isn't missed.
    let shutdown = shutdown();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            Some(event) = inbox.recv() => tree.handle(event),
            _ = &mut shutdown => break,
        }
    }
    drop(events);
}

/// Resolves on Ctrl-C, or on SIGTERM (systemd stopping the service).
async fn shutdown() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .unwrap_or_else(|_| panic!("bonsai: can't listen for SIGTERM"));
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

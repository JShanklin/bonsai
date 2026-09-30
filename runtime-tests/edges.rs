//! What branches send to an edge: every message is either carried out or
//! counted as dropped, and the core never waits.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::bonsai::{Edge, EdgeOut, Event, spawn_edge, stats};

/// An edge that takes 5 ms to carry out each message and never receives.
struct Slow {
    done: Arc<AtomicU64>,
}

impl Edge for Slow {
    type In = ();
    type Out = u32;

    async fn recv(&mut self) -> io::Result<()> {
        std::future::pending().await
    }

    async fn execute(&mut self, _: u32) -> io::Result<()> {
        tokio::time::sleep(Duration::from_millis(5)).await;
        self.done.fetch_add(1, Relaxed);
        Ok(())
    }
}

#[tokio::test]
async fn a_saturated_edge_accounts_for_every_message() {
    let done = Arc::new(AtomicU64::new(0));
    let (events, _inbox) = mpsc::channel::<Event<()>>(16);
    let d = done.clone();
    let tx = spawn_edge::<Slow, (), _>(
        "rt_saturated",
        move || {
            let done = d.clone();
            async move { Ok(Slow { done }) }
        },
        events,
        |_| (),
    );
    let mut out = EdgeOut::new("rt_saturated");
    out.connect(tx);

    // Bursts with a yield between them, so the edge's own tasks run and every
    // queue on the way fills: the core sends far faster than 5 ms each.
    const SENT: u64 = 2_000;
    let mut slowest = Duration::ZERO;
    for i in 0..SENT {
        let started = Instant::now();
        out.send(i as u32);
        slowest = slowest.max(started.elapsed());
        if i % 20 == 19 {
            tokio::task::yield_now().await;
        }
    }
    // The core never waits on the edge.
    assert!(
        slowest < Duration::from_millis(5),
        "a send took {slowest:?}"
    );

    // Let the edge finish what it took.
    let mut last = u64::MAX;
    loop {
        tokio::time::sleep(Duration::from_millis(400)).await;
        let now = done.load(Relaxed);
        if now == last {
            break;
        }
        last = now;
    }
    let s = stats::edge("rt_saturated");
    let (accepted, dropped) = (s.sent.load(Relaxed), s.dropped.load(Relaxed));
    let done = done.load(Relaxed);
    assert_eq!(
        accepted + dropped,
        SENT,
        "accepted {accepted}, dropped {dropped}"
    );
    // Nothing accepted goes missing: all of it was carried out.
    assert_eq!(done, accepted, "accepted {accepted}, carried out {done}");
    assert!(dropped > 0, "the queue should have filled");
}

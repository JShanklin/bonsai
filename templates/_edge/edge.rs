//! The {{edge_name}} edge: where the tree meets something outside it.
//! Grown by `bonsai edge add {{edge_name}} --custom`.
//! Log with info!/warn!/debug!: lines are tagged with this edge.

use std::io;

use crate::bonsai::Edge;

/// What {{edge_name}} keeps open: a socket, a port, a device.
pub struct {{EdgeName}} {}

impl {{EdgeName}} {
    /// Setup: open it. An `Err` is retried with backoff.
    pub async fn setup() -> io::Result<Self> {
        Ok({{EdgeName}} {})
    }
}

impl Edge for {{EdgeName}} {
    /// What it receives; the branches linked from it get it as input.
    type In = Vec<u8>;
    /// What branches send it with `out.to_{{edge_name}}(..)`.
    type Out = Vec<u8>;

    /// The next thing it receives. Must be cancel-safe: keep partial data in self.
    async fn recv(&mut self) -> io::Result<Self::In> {
        std::future::pending().await // delete once it reads something
    }

    /// Execute: carry out one thing a branch sent. An `Err` restarts the edge.
    async fn execute(&mut self, out: Self::Out) -> io::Result<()> {
        let _ = out; // delete once it writes
        Ok(())
    }
}

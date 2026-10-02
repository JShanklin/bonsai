use std::io;

use crate::bonsai::Edge;

pub struct {{EdgeName}} {}

impl {{EdgeName}} {
    pub async fn setup() -> io::Result<Self> {
        Ok({{EdgeName}} {})
    }
}

impl Edge for {{EdgeName}} {
    type In = Vec<u8>;
    type Out = Vec<u8>;

    // Must be cancel-safe: keep partial data in self.
    async fn recv(&mut self) -> io::Result<Self::In> {
        std::future::pending().await // delete once it reads something
    }

    async fn execute(&mut self, out: Self::Out) -> io::Result<()> {
        let _ = out; // delete once it writes
        Ok(())
    }
}

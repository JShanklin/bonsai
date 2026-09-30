//! The serial edge: a serial port (a UART, a USB adapter) as packets.
//! Written by `bonsai sync` while the tree has a serial edge (and removed with
//! the last one, with its `tokio-serial` dependency); don't edit it.

use std::io;

use tokio_serial::{SerialPortBuilderExt, SerialStream};

use crate::bonsai::{Edge, Framed, Framing, Packet};

/// A serial edge's settings, from its `[edge.<name>]` table.
#[derive(Clone, Copy, Debug)]
pub struct SerialConfig {
    pub device: &'static str,
    pub baud: u32,
    pub framing: Framing,
}

pub struct Serial(Framed<SerialStream>);

impl Serial {
    pub async fn setup(cfg: SerialConfig) -> io::Result<Self> {
        let port = tokio_serial::new(cfg.device, cfg.baud)
            .open_native_async()
            .map_err(|e| io::Error::other(format!("open {}: {e}", cfg.device)))?;
        Ok(Serial(Framed::new(port, cfg.framing)))
    }
}

impl Edge for Serial {
    type In = Packet;
    type Out = Packet;

    async fn recv(&mut self) -> io::Result<Packet> {
        Ok(Packet::new(self.0.recv().await?))
    }

    async fn execute(&mut self, out: Packet) -> io::Result<()> {
        self.0.send(&out.bytes).await
    }
}

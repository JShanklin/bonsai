# Roots: TCP

A TCP client that connects to a server, reconnects by itself when the link
drops, and exchanges framed messages.

**Needs:** [chapter 7](../foundations/07-roots.md) and the [wire format](wire-format.md).
**Crates:** none beyond those. `std::net::TcpStream` is all you need.

TCP fits when every message must arrive, in order, over a link that can drop:
a base station, a server, a cloud relay.

## Wire it

```sh
bonsai branch --roots station
bonsai tap station Reading
bonsai tap station Alarm
bonsai release station Setpoint
```

## Fill in `src/branches/station.rs`

Imports:

```rust
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::wire::{self, Wire};
```

Types and settings:

```rust
/// A decoded message from the server.
pub type Inbound = Wire;
/// One encoded frame for the server.
pub type Outbound = Vec<u8>;

const SERVER: &str = "127.0.0.1:9100";
const BACKOFF_MIN: Duration = Duration::from_millis(250);
const BACKOFF_MAX: Duration = Duration::from_secs(8);
```

In `start`, the connection comes and goes, so both threads share it as an
`Arc<Mutex<Option<TcpStream>>>` (see
[Sharing with `Arc`](../foundations/02-rust-essentials.md#sharing-with-arc)).
The receive thread connects, and reconnects, and the send thread writes to
whatever connection is live:

```rust
    // The live connection, shared by both threads. `None` while disconnected.
    let conn: Arc<Mutex<Option<TcpStream>>> = Arc::default();
    let writer = conn.clone();
    let (inbox, outbox) = roots::bridge(
        "station",
        move |deliver| {
            let mut backoff = BACKOFF_MIN;
            loop {
                let stream = match TcpStream::connect(SERVER) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("station: {SERVER}: {e} — retrying in {backoff:?}");
                        thread::sleep(backoff);
                        backoff = (backoff * 2).min(BACKOFF_MAX);
                        continue;
                    }
                };
                backoff = BACKOFF_MIN;
                *conn.lock().unwrap() = stream.try_clone().ok();
                for frame in BufReader::new(stream).split(0) {
                    let Ok(mut frame) = frame else { break };
                    frame.push(0); // `split` drops the 0; decoding needs it
                    if let Some(msg) = wire::decode(&mut frame) {
                        deliver(msg);
                    }
                }
                *conn.lock().unwrap() = None;
            }
        },
        move |frame: Outbound| {
            if let Some(stream) = writer.lock().unwrap().as_mut() {
                let _ = stream.write_all(&frame);
            }
        },
    );
```

`run` is the same as in the [UDP guide](roots-udp.md#fill-in-srcbranchesuplinkrs):
release `Setpoint` from a `Wire::Setpoint`, and `outbox.send(wire::encode(..))`
each tapped nutrient. It doesn't know or care which transport it's on.

## How it behaves

- **Server down:** the receive thread retries after 250 ms, 500 ms, 1 s …
  up to 8 s. Waiting longer each time avoids hammering a server that's
  restarting.
- **Link drops:** reading fails, the shared connection is cleared, and the
  receive thread reconnects. Outgoing frames are dropped until it's back. The
  tree keeps running either way.
- **Slow link:** the send thread waits on the socket, never the executor. If
  the outbox (64 frames) fills, `outbox.send` drops the newest.

## Try it

```sh
nc -l 9100 | od -An -tx1   # a server; stop it and start it again to watch reconnects
cargo local
```

```
station: 127.0.0.1:9100: Connection refused (os error 111) — retrying in 250ms
station: 127.0.0.1:9100: Connection refused (os error 111) — retrying in 500ms
```

## Variations

- **TLS:** wrap the stream with [rustls](https://docs.rs/rustls) before
  splitting. The rest is unchanged.
- **Be the server instead:** `TcpListener::bind(..)`, then `for stream in
  listener.incoming()` in place of the connect loop.

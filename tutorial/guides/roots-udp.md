# Roots: UDP

Send readings to another computer and take setpoints back, over UDP, in a
compact binary format.

**Needs:** [chapter 7](../foundations/07-roots.md) (how a root works) and the
[wire format](wire-format.md).
**Crates:** none beyond those. `std::net::UdpSocket` is all you need.

UDP fits when messages are small and independent, and a lost one doesn't
matter because the next reading replaces it. There's no connection to manage
and no framing to do: one datagram is one message.

## Wire it

```sh
bonsai branch --roots uplink
bonsai tap uplink Reading
bonsai tap uplink Alarm
bonsai release uplink Setpoint
```

## Fill in `src/branches/uplink.rs`

Imports, at the top:

```rust
use std::net::UdpSocket;
use std::sync::Arc;

use crate::wire::{self, Wire};
```

The message types. The receive thread decodes, so `run` gets a ready `Wire`.
`run` encodes, so the send thread only writes bytes:

```rust
/// A decoded message from the peer.
pub type Inbound = Wire;
/// One encoded frame for the peer.
pub type Outbound = Vec<u8>;

const LISTEN: &str = "0.0.0.0:9001";
const SEND_TO: &str = "127.0.0.1:9000";
```

In `start`, replace the `TODO` lines and both closures:

```rust
    let socket = Arc::new(UdpSocket::bind(LISTEN).expect("uplink: can't listen on 9001"));
    let sender = socket.clone();
    let (inbox, outbox) = roots::bridge(
        "uplink",
        move |deliver| {
            let mut buf = [0u8; 256];
            loop {
                if let Ok(n) = socket.recv(&mut buf)
                    && let Some(msg) = wire::decode(&mut buf[..n])
                {
                    deliver(msg);
                }
            }
        },
        move |frame: Outbound| {
            let _ = sender.send_to(&frame, SEND_TO);
        },
    );
```

A datagram that doesn't decode is skipped. In `run`, release the setpoint
(and delete the placeholders):

```rust
            Either::First(msg) => {
                if let Wire::Setpoint { temp_c10 } = msg {
                    sap.release(Nutrient::Setpoint { temp_c10 }).await;
                }
                // bonsai:emit
            }
```

and encode each tapped nutrient:

```rust
                    Nutrient::Reading { temp_c10, humidity } => {
                        outbox.send(wire::encode(&Wire::Reading { temp_c10, humidity }));
                    }
                    Nutrient::Alarm { temp_c10 } => {
                        outbox.send(wire::encode(&Wire::Alarm { temp_c10 }));
                    }
```

## A warning to act on

```
warning: `Alarm` is directed but tapped by display and uplink — each nutrient reaches
         only one of them. Make it broadcast if they should all see it.
```

Both `display` and `uplink` need every alarm:

```sh
bonsai path Alarm broadcast
```

## Try it

Terminal 1, listen for readings (shown as hex; each ends in `00`):

```sh
nc -u -l 9000 | od -An -tx1
```

```
 01 04 86 04 37 00 01 04 98 04 37 00 01 04 aa 04
```

Terminal 2:

```sh
cargo local
```

Send a setpoint of 40.0 °C. `0402a00600` is `Wire::Setpoint { temp_c10: 400 }`
encoded; in your own peer program, use `wire::encode`:

```sh
printf '\x04\x02\xa0\x06\x00' | nc -u -w0 127.0.0.1 9001
```

The alarms stop until the limit changes again.

## Variations

- **Reply to whoever sent last:** use `recv_from`, which also returns the
  sender's address. Keep it in an `Arc<Mutex<Option<SocketAddr>>>` shared with
  the send closure, and `send_to` that.
- **Multicast:** after `bind`, call
  `socket.join_multicast_v4(&"239.1.2.3".parse().unwrap(), &Ipv4Addr::UNSPECIFIED)`
  and send to the group address.
- **Bigger messages:** raise `buf`. A datagram larger than the buffer is cut
  off without an error.

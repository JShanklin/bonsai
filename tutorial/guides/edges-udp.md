# Edges: UDP

Send and receive datagrams: unicast to a fixed address, replies to whoever
sent, and multicast groups (ATAK's situational awareness, many discovery
protocols).

**Needs:** [chapter 7](../foundations/07-edges.md).

UDP fits when messages are small and independent, and a lost one doesn't
matter because the next one replaces it. There's no connection to manage:
one datagram is one `Packet`.

## Settings

```sh
bonsai edge add <name> udp --bind 0.0.0.0:6969 [--to HOST:PORT] [--reply] [--join GROUP …] [--iface ADDR]
```

| key | means |
|-----|-------|
| `bind` | the local address and port it listens on (`0.0.0.0` = every interface) |
| `to` | where a packet goes when it names no `peer` |
| `reply` | with no `to`, send back to whoever sent last |
| `join` | multicast groups to join (repeat `--join` for several) |
| `iface` | the interface address to join them on (default `0.0.0.0`: the system picks) |

A packet out goes to its `peer` if it has one (`packet.reply(..)` sets it to
the sender), else to `to`, else to the last sender when `reply = true`, else
nowhere.

## Multicast in, replies out

A tracker that hears everyone on a multicast group and answers each sender
directly:

```sh
bonsai edge add sa udp --bind 0.0.0.0:6969 --join 239.2.3.1
bonsai branch add tracker
bonsai wire sa tracker
bonsai wire tracker sa
```

```
added udp edge sa
…
sa --> tracker: what sa receives arrives as `Input::Sa(..)`
updated src/wiring.rs
tracker --> sa: send from tracker with `out.to_sa(..)`
updated src/wiring.rs
```

`bonsai.toml`:

```toml
[edge.sa]
kind = "udp"
bind = "0.0.0.0:6969"
join = ["239.2.3.1"]
```

`src/branches/tracker.rs` counts packets per sender in a `HashMap` (a
lookup table: `entry(key).or_insert(0)` finds a sender's count, starting it
at 0 the first time):

```rust
use std::collections::HashMap;
use std::net::SocketAddr;

use crate::bonsai::Branch;
#[allow(unused_imports)] // the messages it sends, and units
use crate::messages::*;
use crate::wiring::tracker::{Input, Out};

/// What tracker keeps between inputs.
pub struct Tracker {
    /// Packets heard from each sender.
    heard: HashMap<SocketAddr, u32>,
}

impl Branch for Tracker {
    type Input = Input;
    type Out = Out;

    fn setup() -> Self {
        Tracker {
            heard: HashMap::new(),
        }
    }

    fn process(&mut self, input: Input, out: &mut Out) {
        match input {
            // `bonsai wire <from> <Message> tracker` adds an arm here
            Input::Sa(packet) => {
                if let Some(from) = packet.peer {
                    let count = self.heard.entry(from).or_insert(0);
                    *count += 1;
                    info!("{} bytes from {from} ({count} so far)", packet.bytes.len());
                    out.to_sa(packet.reply(format!("seen {count}\n")));
                }
            }
            // bonsai:input-arm
        }
    }
}
```

## Try it

Run the tree (`cargo local`), and from another terminal send two datagrams
to the group:

```sh
(echo hello; sleep 1; echo again) | socat - UDP-DATAGRAM:239.2.3.1:6969
```

```
seen 1
seen 2
```

```
10:52:26.550Z  INFO bonsai: running
10:52:26.550Z  INFO sa: up
10:52:28.008Z  INFO tracker: 6 bytes from 192.0.2.2:39005 (1 so far)
10:52:29.007Z  INFO tracker: 6 bytes from 192.0.2.2:39005 (2 so far)
```

## Multicast on a Pi

- **Pick the interface.** On a Pi with Wi-Fi and Ethernet, or a VPN like
  Tailscale, the system may join the group on the wrong one. Set `iface` to
  the address of the interface the group arrives on:
  `--iface 192.168.1.40`.
- **Wi-Fi drops multicast** on many networks (guest networks, some access
  points and office Wi-Fi). If nothing arrives, check with `tcpdump -i wlan0
  udp port 6969` on the Pi. If the packets never show up there, the network
  is dropping them: send unicast to the Pi's address instead, and bind
  `0.0.0.0` so it's heard on every interface.
- **A virtual Pi** on Wi-Fi needs a relay for multicast; see the
  [virtual Pi guide](virtual-pi.md).

## Test it without a network

A test hands the core a packet with a sender, and reads what went out:

```rust
#[cfg(test)]
mod tests {
    use crate::bonsai::{Event, Packet, Tree};
    use crate::wiring::{Core, EdgeIn};

    #[test]
    fn answers_each_sender() {
        let mut core = Core::new();
        let from = "10.0.0.7:5000".parse().ok();
        let hello = Packet { bytes: b"hello".to_vec(), peer: from };
        core.handle(Event::Edge(EdgeIn::Sa(hello)));
        assert_eq!(core.drain_sa(), [Packet { bytes: b"seen 1\n".to_vec(), peer: from }]);
    }
}
```

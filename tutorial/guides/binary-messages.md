# Binary messages

Turn what crosses the network into compact bytes and back, with no
hand-written parsing.

**Needs:** [chapter 7](../foundations/07-edges.md).
**Crates:** [serde](https://serde.rs) (describe the messages) +
[postcard](https://docs.rs/postcard) (a compact binary format, also used on
microcontrollers), with COBS framing for byte streams (built into postcard:
each frame ends in a `0` byte and contains no other).

Why this combination: an `Alarm` is 3 bytes, the same format
works on a microcontroller at the other end of a serial line, and a
corrupted frame on a stream is simply skipped: the next one starts after
the next `0`.

## Messages vs payloads

Chapter 7's greenhouse speaks text (`limit 280`). Here it speaks postcard
instead, and the decoding still happens in the watchdog's `process`, where
tests can reach it. Keep a separate `Payload` type for it rather than sending
your messages as they are:

| | messages (`src/messages.rs`) | `Payload` (`src/codec.rs`) |
|---|---|---|
| between | branches in one tree | your tree and the other end |
| travels as | Rust values in memory | postcard bytes on an edge |
| changing it | free: bonsai regenerates `src/links.rs`, everything recompiles | both ends must agree |
| contains | everything, including messages only branches use | only what the other end should see |

The other end is rarely rebuilt from the same commit as the tree, so the
protocol deserves its own small file you can share with it.

## Add the crates

```sh
cargo add serde --no-default-features --features derive
cargo add postcard --no-default-features --features use-std
```

## `src/codec.rs`

List everything that goes out as bytes in one enum, and add `mod codec;`
after `mod branches;` in `src/main.rs`:

```rust
//! What crosses the network, as bytes. Share this file with the program at
//! the other end.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub enum Payload {
    Alarm { temp_c10: i16 },
    Limit { temp_c10: i16 },
    LimitSet { temp_c10: i16 },
}

/// One message → one datagram.
pub fn encode(message: &Payload) -> Vec<u8> {
    // Only fails for types serde can't describe; a Payload always encodes.
    postcard::to_stdvec(message).unwrap_or_default()
}

/// One datagram → one message; None if it isn't one.
pub fn decode(bytes: &[u8]) -> Option<Payload> {
    postcard::from_bytes(bytes).ok()
}
```

`#[derive(Serialize, Deserialize)]` is what lets serde turn a `Payload` into
postcard bytes and back.

## Use it in a branch

Over UDP, one datagram is one message. Inside the tree a temperature is a
`Celsius`; as bytes it's a whole number of tenths of a degree, which
postcard stores in 2 bytes or less. The watchdog converts at the edge of the
tree. With `use crate::codec::{self, Payload};` at the top, its arms become:

```rust
Input::Reading(reading) => {
    if reading.temp > self.limit {
        out.send(Alarm { temp: reading.temp });
        let alarm = Payload::Alarm {
            temp_c10: tenths(reading.temp),
        };
        out.to_uplink(Packet::new(codec::encode(&alarm)));
    }
}
Input::Uplink(packet) => match codec::decode(&packet.bytes) {
    Some(Payload::Limit { temp_c10 }) => {
        self.limit = Celsius(temp_c10 as f32 / 10.0);
        info!("limit is now {:.1}", self.limit);
        let answer = Payload::LimitSet { temp_c10 };
        out.to_uplink(packet.reply(codec::encode(&answer)));
    }
    other => warn!("didn't expect {other:?}"),
},
```

with, below the `impl`:

```rust
/// As bytes, a temperature is a whole number of tenths: 2 bytes or less.
fn tenths(temp: Celsius) -> i16 {
    (temp.0 * 10.0).round() as i16
}
```

`Payload::Limit { temp_c10: 280 }` is 3 bytes: the variant's number (1), then
280, stored small (`b0 04`). Send those, and the answer is `LimitSet` (2):

```sh
printf '\x01\xb0\x04' | socat - UDP:127.0.0.1:6969 | od -An -tx1
```

```
 02 b0 04
```

```
12:01:08.474Z  INFO watchdog: limit is now 28.0 °C
```

## Byte streams: TCP and serial

A stream has no datagrams: one read can hold half a message, or two. Frame
each message with COBS (it then ends in a `0`), and keep what's been read
until a frame is whole. Add to `src/codec.rs`:

```rust
/// One message → one frame for a byte stream (TCP, serial): it ends in a 0
/// byte, and holds no other.
pub fn encode_frame(message: &Payload) -> Vec<u8> {
    postcard::to_stdvec_cobs(message).unwrap_or_default()
}

/// Frames arriving on a byte stream, which may split or join them.
#[derive(Default)]
pub struct Frames {
    pending: Vec<u8>,
}

impl Frames {
    /// Add what was read; get every whole message it completes. A corrupt
    /// frame is skipped: the next one starts after the next 0.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Payload> {
        self.pending.extend_from_slice(bytes);
        let mut messages = Vec::new();
        while let Some(end) = self.pending.iter().position(|&b| b == 0) {
            let mut frame: Vec<u8> = self.pending.drain(..=end).collect();
            if let Ok(message) = postcard::from_bytes_cobs(&mut frame) {
                messages.push(message);
            }
        }
        messages
    }
}
```

Keep a `Frames` in the branch's struct (`frames: Frames::default()` in
`setup`), and in the stream edge's arm, `for message in
self.frames.push(&packet.bytes) { … }`. Use `framing = "raw"` on the edge:
the frames are yours to cut.

## Check it

At the bottom of `src/codec.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let message = Payload::Alarm { temp_c10: 310 };
        assert_eq!(encode(&message), [0, 236, 4]);
        assert_eq!(decode(&encode(&message)), Some(message));
    }

    #[test]
    fn frames_split_across_reads() {
        let mut stream = encode_frame(&Payload::Limit { temp_c10: 280 });
        stream.extend(encode_frame(&Payload::Alarm { temp_c10: 310 }));
        let mut frames = Frames::default();
        assert_eq!(frames.push(&stream[..3]), []);
        assert_eq!(
            frames.push(&stream[3..]),
            [Payload::Limit { temp_c10: 280 }, Payload::Alarm { temp_c10: 310 }]
        );
    }
}
```

```
test codec::tests::frames_split_across_reads ... ok
test codec::tests::round_trip ... ok
```

## Changing it

Add new variants at the **end** of `Payload`. postcard numbers variants by
position, so reordering them, or removing one from the middle, makes an
older peer read one message as another.

A program without `Vec` (a microcontroller at the other end) encodes into a
buffer instead:

```rust
let mut buf = [0u8; 32];
let frame: &mut [u8] = postcard::to_slice_cobs(&message, &mut buf)?;
```

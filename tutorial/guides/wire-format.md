# Wire format

Turn messages into compact bytes and back, with no hand-written parsing.

**Crates:** [serde](https://serde.rs) (describe the messages) +
[postcard](https://docs.rs/postcard) (a compact binary format built for
embedded) + COBS framing (built into postcard: each frame ends in a `0` byte
and contains no other).

Why this combination: a `Reading` is 5 bytes on the wire, encoding needs no
allocation on an MCU, and a corrupted frame is simply skipped. The next frame
starts after the next `0`.

## `Wire` vs `Nutrient`

They look alike but do different jobs. The root branch translates between
them: an incoming `Wire::Setpoint` becomes a `Nutrient::Setpoint`, and a
tapped `Nutrient::Reading` goes out as a `Wire::Reading`.

| | `Nutrient` (`src/trunk.rs`) | `Wire` (`src/wire.rs`) |
|---|---|---|
| between | branches in one program | your program and the peer |
| travels as | Rust values in memory | postcard bytes on a link |
| can hold | anything: `Box`, handles, driver types | only data that serializes |
| changing it | free: bonsai rewires, everything recompiles | both sides must agree |
| contains | everything, including internal messages like `Beat` | only what the peer should see |

**Why not derive `Serialize` on `Nutrient` and send that?** You can, but
then your internal message list *is* your protocol:

- postcard numbers variants by position. `bonsai starve` on a variant in the
  middle renumbers the rest, and an older peer silently reads one message as
  another.
- Internal messages (`Beat`, branch-to-branch coordination) leak into the
  protocol. `Nutrient` also can't hold anything that doesn't serialize.
- The peer has to compile your whole `Nutrient` enum, instead of one small
  shared file.

Keep `Wire` separate whenever the other end isn't rebuilt and deployed with
the tree, which in the field is almost always. Serializing `Nutrient` directly
is fine for a quick prototype where both ends always come from the same
commit.

## Add the crates

Pi / PC trees (std):

```sh
cargo add serde --no-default-features --features derive
cargo add postcard --no-default-features --features use-std
```

Pico / ESP32 trees (no_std), without `use-std`:

```sh
cargo add serde --no-default-features --features derive
cargo add postcard --no-default-features
```

## `src/wire.rs`

List everything that crosses the wire in one enum, and share this file with
the program on the other end:

```rust
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub enum Wire {
    Reading { temp_c10: i16, humidity: u8 },
    Alarm { temp_c10: i16 },
    Setpoint { temp_c10: i16 },
}

/// One message → one frame (ends in a 0 byte).
pub fn encode(msg: &Wire) -> Vec<u8> {
    postcard::to_stdvec_cobs(msg).expect("a Wire always serializes")
}

/// One frame (including its 0 byte) → one message; None if it's corrupt.
pub fn decode(frame: &mut [u8]) -> Option<Wire> {
    postcard::from_bytes_cobs(frame).ok()
}
```

Add `mod wire;` next to `mod trunk;` in `src/main.rs`.

On an MCU, where there's no `Vec`, encode into a buffer instead:

```rust
let mut buf = [0u8; 32];
let frame: &mut [u8] = postcard::to_slice_cobs(&msg, &mut buf)?;
```

## Check it

Add to the bottom of `src/wire.rs` and run `cargo test`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let msg = Wire::Reading { temp_c10: 257, humidity: 55 };
        let mut frame = encode(&msg);
        assert_eq!(decode(&mut frame), Some(msg));
    }
}
```

## Use it

- **Byte streams (TCP, serial):** split incoming bytes at each `0` and
  `decode` each frame. The [TCP](roots-tcp.md) and [serial](roots-serial.md)
  guides show the loop.
- **Datagrams (UDP):** one datagram = one frame. See the [UDP](roots-udp.md)
  guide.
- **Changing messages:** add new variants at the **end** of `Wire`. postcard
  numbers variants by position, so reordering them breaks older peers.

# Testing

A branch's `process` does no I/O and never waits, so tests need no
network, no timers and no hardware: hand it inputs, check what it sent. The
core can be tested the same way, edges included.

**Needs:** [chapter 6](../foundations/06-order-and-tests.md).

Run the tests on your computer:

```sh
cargo local-test    # a Pi tree (plain `cargo test` would build them for the Pi)
cargo test          # a host tree
```

## One branch

Make it with `setup`, give it an input and an empty `Out`, and read
`out.sent()`: everything it sent, oldest first, as `Msg`s (one variant per
wire, named after the sender and what it sends: `WatchdogAlarm`,
`WatchdogToUplink`).

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::wiring::Msg;

    #[test]
    fn alarms_only_above_the_limit() {
        let mut watchdog = Watchdog::setup();

        let mut out = Out::default();
        let reading = Reading {
            temp_c10: 300,
            humidity: 55,
        };
        watchdog.process(Input::Reading(reading), &mut out);
        assert!(out.sent().is_empty());

        let mut out = Out::default();
        let reading = Reading {
            temp_c10: 310,
            humidity: 55,
        };
        watchdog.process(Input::Reading(reading), &mut out);
        assert!(matches!(
            out.sent(),
            [
                Msg::WatchdogAlarm(Alarm { temp_c10: 310 }),
                Msg::WatchdogToUplink(_)
            ]
        ));
    }
}
```

Messages derive `Debug` but not `PartialEq`, so compare them with
`matches!` and a pattern, or add `PartialEq` to a message's `#[derive(..)]`
in `src/messages.rs` and use `assert_eq!`.

A branch's state carries over between inputs, so a sequence is just a loop
([chapter 6](../foundations/06-order-and-tests.md#test-a-branch) tests the
sensor over eight ticks).

## The whole tree

`Core::new()` sets up every branch; `core.handle(event)` runs one event to
completion, exactly as the running tree does. With no edges started, what
branches send an edge is kept, and `core.drain_<edge>()` returns it:

```rust
#[cfg(test)]
mod tests {
    use crate::bonsai::{Event, Packet, Tree};
    use crate::wiring::{Core, EdgeIn};

    #[test]
    fn a_lower_limit_sets_off_an_alarm() {
        let mut core = Core::new();
        let limit = Packet::new("limit 260");
        core.handle(Event::Edge(EdgeIn::Uplink(limit)));
        core.handle(Event::Tick(0)); // sensor, the first branch: 26.5 °C
        assert_eq!(
            core.drain_uplink(),
            [Packet::new("ok, limit 260\n"), Packet::new("alarm 265\n")]
        );
    }
}
```

- `Event::Edge(EdgeIn::<Edge>(value))` is something an edge received.
  For built-in edges, `value` is a `Packet`; set `peer` to test replies
  (`Packet { bytes: b"hello".to_vec(), peer: "10.0.0.7:5000".parse().ok() }`).
- `Event::Tick(n)` is a tick for branch `n`, counting from 0 in
  `bonsai.toml`'s order.

Put tree-wide tests at the bottom of `src/main.rs`.

## Logs in tests

`info!` and friends work in tests. `cargo test` captures their lines and
shows them only for a test that fails, next to its error.

## What not to test

bonsai's plumbing (delivery order, edges restarting, the core loop) is
bonsai's job; the six tests you'll see from `src/bonsai.rs` in every tree
are its own. Test your decisions: what each branch does with each input,
and what the tree does with a sequence of events.

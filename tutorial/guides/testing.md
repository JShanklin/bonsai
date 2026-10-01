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

`bonsai link` and `bonsai rate` write a test for each input they give a
branch (`on_reading`, `on_tick`, …, in the `mod tests` at the bottom of its
file): it hands the branch one such input and checks that it sends nothing.
Change its last line to say what it should send. An input with no obvious
value to start from (a custom edge's, or a message with a field of your own
type) gets `todo!` and `#[ignore]` until you fill it in. Each sits between
`// bonsai:test on_<input> begin <n>` and `… end` lines; `unlink` removes
the block with the arm only while it's as bonsai wrote it. An edited test,
or one from before the markers, is kept, with a note saying what to update.

Make it with `setup`, give it an input and an empty `Out`, and read
`out.sent()`: everything it sent, oldest first, as `Msg`s (one variant per
link, named after the sender and what it sends: `WatchdogAlarm`,
`WatchdogToUplink`).

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::links::Msg;

    #[test]
    fn alarms_only_above_the_limit() {
        let mut watchdog = Watchdog::setup();

        let mut out = Out::default();
        let reading = Reading {
            temp: Celsius(30.0),
            humidity: Percent(55.0),
        };
        watchdog.process(Input::Reading(reading), &mut out);
        assert!(out.sent().is_empty());

        let mut out = Out::default();
        let reading = Reading {
            temp: Celsius(31.0),
            humidity: Percent(55.0),
        };
        watchdog.process(Input::Reading(reading), &mut out);
        assert!(matches!(
            out.sent(),
            [Msg::WatchdogAlarm(alarm), Msg::WatchdogToUplink(_)]
                if alarm.temp == Celsius(31.0)
        ));
    }
}
```

Messages derive `Debug` but not `PartialEq`, so compare them with
`matches!`, a pattern and a guard on the fields (units compare with `==`),
or add `PartialEq` to a message's `#[derive(..)]` in `src/messages.rs` and
use `assert_eq!`.

A branch's state carries over between inputs, so a sequence is just a loop
([chapter 6](../foundations/06-order-and-tests.md#test-a-branch) tests the
sensor over eight ticks).

## The whole tree

`Core::new()` sets up every branch. Each of these runs one event to
completion, exactly as the running tree does:

- `core.tick_<branch>()`: one tick of a branch's rate (there's one for each
  branch with a `rate`).
- `core.from_<edge>(value)`: something an edge received. For built-in edges,
  `value` is a `Packet`; set `peer` to test replies (`Packet { bytes:
  b"hello".to_vec(), peer: "10.0.0.7:5000".parse().ok() }`).

With no edges started, what branches send an edge is kept, and
`core.drain_<edge>()` returns it:

```rust
#[cfg(test)]
mod tests {
    use crate::bonsai::Packet;
    use crate::links::Core;

    #[test]
    fn a_lower_limit_sets_off_an_alarm() {
        let mut core = Core::new();
        core.from_uplink(Packet::new("limit 26"));
        core.tick_sensor(); // 26.5 °C
        assert_eq!(
            core.drain_uplink(),
            [Packet::new("ok, limit 26.0 °C\n"), Packet::new("alarm 26.5\n")]
        );
    }
}
```

A sequence is a loop: replay a list of what came in, in order, and check
what went out:

```rust
for line in ["limit 26", "hello", "limit 30"] {
    core.from_uplink(Packet::new(line));
}
```

They're named after the branches and edges, so a test that ticks a branch
whose rate was taken off, or feeds an edge that's gone, doesn't compile.
(Underneath, each is `core.handle(event)` with an `Event::Tick(n)`, `n`
counting branches from 0 in `bonsai.toml`'s order, or an
`Event::Edge(EdgeIn::<Edge>(value))`.)

Put tree-wide tests at the bottom of `src/main.rs`.

## Logs in tests

`info!` and friends work in tests. `cargo test` captures their lines and
shows them only for a test that fails, next to its error.

## What not to test

bonsai's plumbing (delivery order, edges restarting, the core loop) is
bonsai's job; the nine tests you'll see from `src/bonsai.rs` in every tree
are its own. Test your decisions: what each branch does with each input,
and what the tree does with a sequence of events.

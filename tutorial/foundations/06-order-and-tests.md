# 6. Order and tests

The greenhouse works. This chapter is about why you can rely on it: the
order things happen in, and how to test a branch without running the tree.

## The order is fixed

When the sensor ticks, the core:

1. hands `Input::Tick` to the sensor, which sends a `Reading`;
2. delivers that `Reading` to each branch in its link's `to` list, in order:
   the watchdog first, which sends an `Alarm`, then the display;
3. delivers the `Alarm` to the display;
4. finds nothing left, and takes the next event.

Messages are delivered oldest first, which is why the display logs the
reading before the alarm. Nothing else runs in between: not another tick,
not a packet from the network. The same inputs, in the same order, always
give the same outputs.

Branches run in the order of `bonsai.toml`'s tables, and each link's
receivers in the order of its `to` list. Change the order there (and run
`bonsai sync`) to change it in the tree.

## Loops

A branch can send to a branch that sends back. bonsai allows it, but warns,
because a loop where every send is unconditional never ends:

```sh
bonsai link display Alarm sensor
```

```
display --Alarm--> sensor: send it from display with `out.send(Alarm { .. })`
updated src/wiring.rs
warning: sensor → display → sensor send to each other in a loop: make at least one of those sends conditional
warning: sensor → watchdog → display → sensor send to each other in a loop: make at least one of those sends conditional
```

The greenhouse's sends are conditional (the watchdog only alarms above the
limit), but it doesn't need this link, so take it out again:

```sh
bonsai unlink display Alarm sensor
```

```
unlinked display from sensor; take its `out.send(Alarm ..)` out of display
updated src/wiring.rs
```

`unlink` also removed the `Input::Alarm` arm it had added to the sensor. If
a loop does run away, the core stops it after 10,000 messages from one
event, logs an error, and goes on with the next event.

## Test a branch

`process` is an ordinary function with no I/O, so a test can call it
directly: make the branch with `setup`, hand it an input, look at what it
sent with `out.sent()`. Add this to the bottom of
`src/branches/watchdog.rs`:

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
            [Msg::WatchdogAlarm(alarm)] if alarm.temp == Celsius(31.0)
        ));
    }
}
```

What a branch sends is a list of `Msg`s, one variant per link, named after
the sender and the message: `WatchdogAlarm` is the watchdog's `Alarm` link.

And one for the sensor, at the bottom of `src/branches/sensor.rs`. Its state
carries over from one input to the next, so drive it for several ticks:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::wiring::Msg;

    #[test]
    fn warms_then_starts_over() {
        let mut sensor = Sensor::setup();
        let mut temps = Vec::new();
        for _ in 0..8 {
            let mut out = Out::default();
            sensor.process(Input::Tick, &mut out);
            if let [Msg::SensorReading(reading)] = out.sent() {
                temps.push(reading.temp.0);
            }
        }
        assert_eq!(temps, [26.5, 28.0, 29.5, 31.0, 32.5, 34.0, 25.0, 26.5]);
    }
}
```

Run the tests on your computer (plain `cargo test` would build them for the
Pi):

```sh
cargo local-test
```

```
running 11 tests
test bonsai::stats::tests::a_snapshot_renders_as_tab_separated_rows ... ok
test bonsai::log::tests::filter_takes_a_default_and_per_source_levels ... ok
test bonsai::log::tests::a_branch_being_processed_tags_its_lines ... ok
test bonsai::log::tests::lines_carry_utc_time_level_and_source ... ok
test bonsai::stats::tests::the_same_name_gets_the_same_counts ... ok
test bonsai::top::tests::bonsai_top_picks_the_address ... ok
test bonsai::units::tests::a_kind_converts_both_ways ... ok
test bonsai::units::tests::units_multiply_into_others ... ok
test bonsai::units::tests::units_print_with_their_symbol ... ok
test branches::sensor::tests::warms_then_starts_over ... ok
test branches::watchdog::tests::alarms_only_above_the_limit ... ok

test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

The first nine are bonsai's own, in `src/bonsai.rs`. Chapter 7 adds a test
that drives the whole tree. The [testing guide](../guides/testing.md) has
more.

```sh
git add -A && git commit -m "Tests"
```

Next: [Edges](07-edges.md).

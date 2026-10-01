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
updated src/links.rs
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
updated src/links.rs
```

`unlink` also removed the `Input::Alarm` arm it had added to the sensor. If
a loop does run away, the core stops it after 10,000 messages from one
event, logs an error, and goes on with the next event.

## Test a branch

`process` is an ordinary function with no I/O, so a test can call it
directly: make the branch with `setup`, hand it an input, look at what it
sent with `out.sent()`. bonsai has written one already for each input you
gave a branch: `bonsai link` and `bonsai rate` add a test next to each arm,
in the `mod tests` at the bottom of the branch's file. The watchdog's, for
the `Reading` it receives:

```rust
    #[test]
    fn on_reading() {
        let mut branch = Watchdog::setup();
        let mut out = Out::default();
        let reading = Reading {
            temp: Celsius(0.0),
            humidity: Percent(0.0),
        };
        branch.process(Input::Reading(reading), &mut out);
        // What should this Reading make it send? out.sent() lists it, oldest first.
        // For example: assert!(matches!(out.sent(), [Msg::..]));
        assert!(out.sent().is_empty(), "it sends {:?}", out.sent());
    }
```

It starts every field at zero and checks the branch sends nothing, which
was true when bonsai wrote it. Run the tests on your computer (plain `cargo
test` would build them for the Pi):

```sh
cargo local-test
```

```
test branches::sensor::tests::on_tick ... FAILED

---- branches::sensor::tests::on_tick stdout ----

thread 'branches::sensor::tests::on_tick' (11742) panicked at src/branches/sensor.rs:62:9:
it sends [SensorReading(Reading { temp: Celsius(26.5), humidity: Percent(55.0) })]
```

The sensor sends a reading on every tick now, so its test is out of date,
and says what it sends instead. Make each test say what the branch should
do. The watchdog's: nothing at the limit, an alarm just above it:

```rust
    #[test]
    fn on_reading() {
        let mut branch = Watchdog::setup();

        let mut out = Out::default();
        let reading = Reading {
            temp: Celsius(30.0),
            humidity: Percent(55.0),
        };
        branch.process(Input::Reading(reading), &mut out);
        assert!(out.sent().is_empty(), "it sends {:?}", out.sent());

        let mut out = Out::default();
        let reading = Reading {
            temp: Celsius(31.0),
            humidity: Percent(55.0),
        };
        branch.process(Input::Reading(reading), &mut out);
        assert!(matches!(
            out.sent(),
            [Msg::WatchdogAlarm(alarm)] if alarm.temp == Celsius(31.0)
        ));
    }
```

What a branch sends is a list of `Msg`s, one variant per link, named after
the sender and the message: `WatchdogAlarm` is the watchdog's `Alarm` link.

The sensor's state carries over from one input to the next, so drive it
for several ticks:

```rust
    #[test]
    fn on_tick() {
        let mut branch = Sensor::setup();
        let mut temps = Vec::new();
        for _ in 0..8 {
            let mut out = Out::default();
            branch.process(Input::Tick, &mut out);
            if let [Msg::SensorReading(reading)] = out.sent() {
                temps.push(reading.temp.0);
            }
        }
        assert_eq!(temps, [26.5, 28.0, 29.5, 31.0, 32.5, 34.0, 25.0, 26.5]);
    }
```

Keep the names: if you `unlink` an input later, bonsai takes its arm and
its `on_…` test out together (and says so when you'd changed them). The
display's two tests still pass as written: it logs, and sends nothing.

```sh
cargo local-test
```

```
running 17 tests
test bonsai::log::tests::a_branch_being_processed_tags_its_lines ... ok
test bonsai::log::tests::lines_carry_utc_time_level_and_source ... ok
test bonsai::log::tests::filter_takes_a_default_and_per_source_levels ... ok
test bonsai::record::tests::a_run_ended_when_its_last_line_is_end ... ok
test bonsai::record::tests::durations_read_at_a_glance ... ok
test bonsai::record::tests::names_and_lines_use_local_time ... ok
test bonsai::stats::tests::a_snapshot_renders_as_tab_separated_rows ... ok
test bonsai::stats::tests::proc_files_give_the_process_and_the_computer ... ok
test bonsai::units::tests::a_kind_converts_both_ways ... ok
test bonsai::stats::tests::the_same_name_gets_the_same_counts ... ok
test bonsai::units::tests::units_multiply_into_others ... ok
test bonsai::top::tests::bonsai_top_picks_the_address ... ok
test bonsai::units::tests::units_print_with_their_symbol ... ok
test branches::sensor::tests::on_tick ... ok
test branches::display::tests::on_alarm ... ok
test branches::display::tests::on_reading ... ok
test branches::watchdog::tests::on_reading ... ok

test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

The first thirteen are bonsai's own, in `src/bonsai.rs`. Chapter 7 adds a test
that drives the whole tree. The [testing guide](../guides/testing.md) has
more.

```sh
git add -A && git commit -m "Tests"
```

Next: [Edges](07-edges.md).

# 5. Branches and messages

The greenhouse gets three parts:

- **sensor** reads the temperature once a second.
- **watchdog** raises an alarm when it's too hot.
- **display** shows readings and alarms.

## Branches

```sh
bonsai branch add sensor
bonsai branch add watchdog
bonsai branch add display
```

```
added branch sensor: src/branches/sensor.rs
updated src/links.rs, src/branches/mod.rs
warning: sensor has no inputs, so its process never runs: link something to it, or give it a rate
…
```

Each writes `src/branches/<name>.rs` and adds a `[branch.<name>]` table to
`bonsai.toml`. The warning is fair: nothing reaches these branches yet.

## Messages

A message is a struct with typed fields. Give a measurement its unit
([chapter 2](02-rust-essentials.md#units)):

```sh
bonsai message add Reading temp:Celsius humidity:Percent
bonsai message add Alarm temp:Celsius
```

```
added message Reading to src/messages.rs
added message Alarm to src/messages.rs
```

`src/messages.rs` now has:

```rust
#[derive(Clone, Debug)]
pub struct Reading {
    pub temp: Celsius,
    pub humidity: Percent,
}

#[derive(Clone, Debug)]
pub struct Alarm {
    pub temp: Celsius,
}
```

Add fields by hand whenever you like; bonsai reads the struct names, not
their fields. The units come from the `pub use crate::bonsai::units::*;`
line at the top of the file, which also brings them into every branch.

## Links and a rate

A link says who sends a message to whom. List every receiver; the core
delivers to them in that order:

```sh
bonsai link sensor Reading watchdog display
bonsai link watchdog Alarm display
bonsai rate sensor 1
```

```
sensor --Reading--> watchdog, display: send it from sensor with `out.send(Reading { .. })`
updated src/links.rs
watchdog --Alarm--> display: send it from watchdog with `out.send(Alarm { .. })`
updated src/links.rs
sensor ticks once a second: `Input::Tick` in its process
updated src/links.rs
```

`bonsai rate sensor 1` gives the sensor its own clock: an `Input::Tick` once
a second. Every command above records its change in `bonsai.toml`:

```toml
[branch.sensor]
rate = 1

[branch.watchdog]

[branch.display]

[[link]]
from = "sensor"
message = "Reading"
to = ["watchdog", "display"]

[[link]]
from = "watchdog"
message = "Alarm"
to = ["display"]
```

You can edit `bonsai.toml` by hand too; run `bonsai sync` afterwards to
regenerate `src/links.rs` (`bonsai sync --dry-run` first shows what it would
change, as a diff, and writes nothing). Check the whole graph with:

```sh
bonsai list
```

```
tree: greenhouse  (zero-2w (bcm2710a1))
branches, in the order the core runs them:
  sensor  (ticks 1/s)
  watchdog
  display
links:
  sensor --Reading--> watchdog, display
  watchdog --Alarm--> display
record: events, panics → logs/
```

## What each branch receives

Each link also added an arm to its receivers' `match input` (and a test for
it at the bottom of the file, which [chapter 6](06-order-and-tests.md#test-a-branch)
puts to work). The display's `process` now reads:

```rust
fn process(&mut self, input: Input, out: &mut Out) {
    let _ = out; // delete once it sends
    match input {
        // `bonsai link <from> <Message> display` adds an arm here
        Input::Reading(_reading) => {}
        Input::Alarm(_alarm) => {}
        // bonsai:input-arm
    }
}
```

`Input` is generated (in `src/links.rs`) with exactly what's linked to the
branch. The compiler holds you to it. Delete the `Input::Alarm` arm and
`cargo local` refuses to build:

```
error[E0004]: non-exhaustive patterns: `links::display::Input::Alarm(_)` not covered
   --> src/branches/display.rs:26:15
    |
 26 |         match input {
    |               ^^^^^ pattern `links::display::Input::Alarm(_)` not covered
```

Sending is checked the same way. The sensor is linked to send `Reading` only,
so `out.send(Alarm { temp: Celsius(40.0) })` in the sensor is an error:

```
error[E0308]: mismatched types
  --> src/branches/sensor.rs:28:37
   |
28 |             Input::Tick => out.send(Alarm { temp: Celsius(40.0) }),
   |                                ---- ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ expected `Reading`, found `Alarm`
```

## A setting

The watchdog's limit belongs in `bonsai.toml`, next to the branch, not in
its code. Add a line to its table:

```toml
[branch.watchdog]
limit = 30.0   # too hot above 30 °C
```

```sh
bonsai sync
```

```
updated src/settings.rs
```

Every key in a branch's table except `rate` becomes a constant in
`src/settings.rs`:

```rust
pub mod watchdog {
    pub const LIMIT: f64 = 30.0;
}
```

A setting is a plain number (or text, or `true`/`false`); the branch gives
it its unit.

## Fill them in

Now write what each branch decides. The state a branch keeps goes in its
struct, and `setup` makes the starting value.

`src/branches/sensor.rs`: there's no real sensor yet, so pretend. Each tick
it gets 1.5 °C warmer, until it passes 35 °C and starts over. The `let _ =
out;` line goes, since the sensor sends now:

```rust
/// What sensor keeps between inputs.
pub struct Sensor {
    temp: Celsius,
}

impl Branch for Sensor {
    type Input = Input;
    type Out = Out;

    /// Setup: the starting state. Runs again if `process` panics.
    fn setup() -> Self {
        Sensor {
            temp: Celsius(25.0),
        }
    }

    /// Process: decide what to do with each input, and `out.send(..)` the
    /// result. No I/O and no waiting, so the same inputs give the same outputs.
    /// Log with info!/warn!/debug!: lines are tagged with this branch.
    fn process(&mut self, input: Input, out: &mut Out) {
        match input {
            // `bonsai link <from> <Message> sensor` adds an arm here
            Input::Tick => {
                // A pretend sensor: 1.5 °C warmer each time, then back to 25.
                self.temp += Celsius(1.5);
                if self.temp > Celsius(35.0) {
                    self.temp = Celsius(25.0);
                }
                out.send(Reading {
                    temp: self.temp,
                    humidity: Percent(55.0),
                });
            }
            // bonsai:input-arm
        }
    }
}
```

`src/branches/watchdog.rs`: it keeps the limit, read from the setting in
`setup`. Add `use crate::settings;` under the other `use` lines:

```rust
pub struct Watchdog {
    limit: Celsius,
}

impl Branch for Watchdog {
    type Input = Input;
    type Out = Out;

    fn setup() -> Self {
        Watchdog {
            limit: Celsius(settings::watchdog::LIMIT as f32),
        }
    }

    fn process(&mut self, input: Input, out: &mut Out) {
        match input {
            // `bonsai link <from> <Message> watchdog` adds an arm here
            Input::Reading(reading) => {
                if reading.temp > self.limit {
                    out.send(Alarm { temp: reading.temp });
                }
            }
            // bonsai:input-arm
        }
    }
}
```

Compare `reading.temp > 30.0` instead, and the compiler stops you: a
`Celsius` only compares with a `Celsius` (chapter 2 shows the error).

`src/branches/display.rs` sends nothing, so it keeps `let _ = out;`. It logs
instead, with `info!` and `warn!`. Units print with their symbol:

```rust
fn process(&mut self, input: Input, out: &mut Out) {
    let _ = out; // delete once it sends
    match input {
        // `bonsai link <from> <Message> display` adds an arm here
        Input::Reading(reading) => {
            info!("{:.1}, {:.0} humidity", reading.temp, reading.humidity);
        }
        Input::Alarm(alarm) => warn!("too hot: {:.1}", alarm.temp),
        // bonsai:input-arm
    }
}
```

## Run it

```sh
cargo local
```

```
11:54:50.171Z  INFO bonsai: running
11:54:50.172Z  INFO display: 26.5 °C, 55% humidity
11:54:51.172Z  INFO display: 28.0 °C, 55% humidity
11:54:52.172Z  INFO display: 29.5 °C, 55% humidity
11:54:53.173Z  INFO display: 31.0 °C, 55% humidity
11:54:53.173Z  WARN display: too hot: 31.0 °C
11:54:54.172Z  INFO display: 32.5 °C, 55% humidity
11:54:54.172Z  WARN display: too hot: 32.5 °C
```

Each line names the branch that wrote it. Notice 11:54:53.173: the reading,
*then* the alarm, both from the sensor's one tick. The core did everything
that tick set off before taking the next event. Chapter 6 explains why
that's guaranteed.

**`cargo fmt`** tidies your code, and will move `// bonsai:input-arm` up
behind the last arm (`} // bonsai:input-arm`). That's fine: bonsai puts it
back on its own line the next time it adds an arm.

```sh
git add -A && git commit -m "Sensor, watchdog, display"
```

Next: [Order and tests](06-order-and-tests.md).

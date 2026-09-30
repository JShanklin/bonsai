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
updated src/wiring.rs, src/branches/mod.rs
warning: sensor has no inputs, so its process never runs: wire something to it, or give it a rate
…
```

Each writes `src/branches/<name>.rs` and adds a `[branch.<name>]` table to
`bonsai.toml`. The warning is fair: nothing reaches these branches yet.

## Messages

A message is a struct with typed fields:

```sh
bonsai message add Reading temp_c10:i16 humidity:u8
bonsai message add Alarm temp_c10:i16
```

```
added message Reading to src/messages.rs
added message Alarm to src/messages.rs
```

`src/messages.rs` now has:

```rust
#[derive(Clone, Debug)]
pub struct Reading {
    pub temp_c10: i16,
    pub humidity: u8,
}

#[derive(Clone, Debug)]
pub struct Alarm {
    pub temp_c10: i16,
}
```

Add fields by hand whenever you like; bonsai reads the struct names, not
their fields.

## Wires and a rate

A wire says who sends a message to whom. List every receiver; the core
delivers to them in that order:

```sh
bonsai wire sensor Reading watchdog display
bonsai wire watchdog Alarm display
bonsai rate sensor 1
```

```
sensor --Reading--> watchdog, display: send it from sensor with `out.send(Reading { .. })`
updated src/wiring.rs
watchdog --Alarm--> display: send it from watchdog with `out.send(Alarm { .. })`
updated src/wiring.rs
sensor ticks once a second: `Input::Tick` in its process
updated src/wiring.rs
```

`bonsai rate sensor 1` gives the sensor its own clock: an `Input::Tick` once
a second. Every command above records its change in `bonsai.toml`:

```toml
[branch.sensor]
rate = 1

[branch.watchdog]

[branch.display]

[[wire]]
from = "sensor"
message = "Reading"
to = ["watchdog", "display"]

[[wire]]
from = "watchdog"
message = "Alarm"
to = ["display"]
```

You can edit `bonsai.toml` by hand too; run `bonsai sync` afterwards to
regenerate the wiring. Check the whole graph with:

```sh
bonsai list
```

```
tree: greenhouse  (zero-2w (bcm2710a1))
branches, in the order the core runs them:
  pulse  (ticks 2/s)
  sensor  (ticks 1/s)
  watchdog
  display
wires:
  sensor --Reading--> watchdog, display
  watchdog --Alarm--> display
```

## What each branch receives

Each wire also added an arm to its receivers' `match input`. The display's
`process` now reads:

```rust
fn process(&mut self, input: Input, out: &mut Out) {
    let _ = out; // delete once it sends
    match input {
        // `bonsai wire <from> <Message> display` adds an arm here
        Input::Reading(_reading) => {}
        Input::Alarm(_alarm) => {}
        // bonsai:input-arm
    }
}
```

`Input` is generated (in `src/wiring.rs`) with exactly what's wired to the
branch. The compiler holds you to it. Delete the `Input::Alarm` arm and
`cargo local` refuses to build:

```
error[E0004]: non-exhaustive patterns: `wiring::display::Input::Alarm(_)` not covered
   --> src/branches/display.rs:26:15
    |
 26 |         match input {
    |               ^^^^^ pattern `wiring::display::Input::Alarm(_)` not covered
```

Sending is checked the same way. The sensor is wired to send `Reading` only,
so `out.send(Alarm { temp_c10: 400 })` in the sensor is an error:

```
error[E0308]: mismatched types
  --> src/branches/sensor.rs:28:37
   |
28 |             Input::Tick => out.send(Alarm { temp_c10: 400 }),
   |                                ---- ^^^^^^^^^^^^^^^^^^^^^^^ expected `Reading`, found `Alarm`
```

## A setting

The watchdog's limit belongs in `bonsai.toml`, next to the branch, not in
its code. Add a line to its table:

```toml
[branch.watchdog]
limit_c10 = 300   # too hot above 30.0 °C
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
    pub const LIMIT_C10: i64 = 300;
}
```

## Fill them in

Now write what each branch decides. The state a branch keeps goes in its
struct, and `setup` makes the starting value.

`src/branches/sensor.rs`: there's no real sensor yet, so pretend. Each tick
it gets 1.5 °C warmer, until it passes 35.0 °C and starts over. The `let _ =
out;` line goes, since the sensor sends now:

```rust
/// What sensor keeps between inputs.
pub struct Sensor {
    temp_c10: i16,
}

impl Branch for Sensor {
    type Input = Input;
    type Out = Out;

    /// Setup: the starting state. Runs again if `process` panics.
    fn setup() -> Self {
        Sensor { temp_c10: 250 }
    }

    /// Process: decide what to do with each input, and `out.send(..)` the
    /// result. No I/O and no waiting, so the same inputs give the same outputs.
    /// Log with info!/warn!/debug!: lines are tagged with this branch.
    fn process(&mut self, input: Input, out: &mut Out) {
        match input {
            // `bonsai wire <from> <Message> sensor` adds an arm here
            Input::Tick => {
                // A pretend sensor: 1.5 °C warmer each time, then back to 25.0.
                self.temp_c10 += 15;
                if self.temp_c10 > 350 {
                    self.temp_c10 = 250;
                }
                out.send(Reading {
                    temp_c10: self.temp_c10,
                    humidity: 55,
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
    limit_c10: i16,
}

impl Branch for Watchdog {
    type Input = Input;
    type Out = Out;

    fn setup() -> Self {
        Watchdog {
            limit_c10: settings::watchdog::LIMIT_C10 as i16,
        }
    }

    fn process(&mut self, input: Input, out: &mut Out) {
        match input {
            // `bonsai wire <from> <Message> watchdog` adds an arm here
            Input::Reading(reading) => {
                if reading.temp_c10 > self.limit_c10 {
                    out.send(Alarm {
                        temp_c10: reading.temp_c10,
                    });
                }
            }
            // bonsai:input-arm
        }
    }
}
```

`src/branches/display.rs` sends nothing, so it keeps `let _ = out;`. It logs
instead, with `info!` and `warn!`:

```rust
fn process(&mut self, input: Input, out: &mut Out) {
    let _ = out; // delete once it sends
    match input {
        // `bonsai wire <from> <Message> display` adds an arm here
        Input::Reading(reading) => {
            let temp = reading.temp_c10 as f32 / 10.0;
            info!("{temp:.1} °C, {}% humidity", reading.humidity);
        }
        Input::Alarm(alarm) => {
            let temp = alarm.temp_c10 as f32 / 10.0;
            warn!("too hot: {temp:.1} °C");
        }
        // bonsai:input-arm
    }
}
```

## Run it

```sh
cargo local
```

```
10:41:46.537Z  INFO bonsai: running
10:41:46.538Z  INFO pulse: beat
10:41:46.538Z  INFO display: 26.5 °C, 55% humidity
10:41:47.038Z  INFO pulse: beat
10:41:47.538Z  INFO display: 28.0 °C, 55% humidity
10:41:47.538Z  INFO pulse: beat
10:41:48.038Z  INFO pulse: beat
10:41:48.538Z  INFO display: 29.5 °C, 55% humidity
10:41:48.538Z  INFO pulse: beat
10:41:49.038Z  INFO pulse: beat
10:41:49.538Z  INFO display: 31.0 °C, 55% humidity
10:41:49.538Z  WARN display: too hot: 31.0 °C
10:41:49.538Z  INFO pulse: beat
```

Each line names the branch that wrote it. Notice the order at 10:41:49.538:
the reading, *then* the alarm, *then* the next beat. The sensor's one tick
set off everything the tree did about it before anything else happened.
Chapter 6 explains why that's guaranteed.

## Prune the pulse

The display now shows the tree is alive, so the pulse can go:

```sh
bonsai branch remove pulse
bonsai list
```

```
removed branch pulse
updated src/wiring.rs, src/branches/mod.rs
tree: greenhouse  (zero-2w (bcm2710a1))
branches, in the order the core runs them:
  sensor  (ticks 1/s)
  watchdog  (settings: limit_c10)
  display
wires:
  sensor --Reading--> watchdog, display
  watchdog --Alarm--> display
```

```
10:42:01.672Z  INFO bonsai: running
10:42:01.673Z  INFO display: 26.5 °C, 55% humidity
10:42:02.674Z  INFO display: 28.0 °C, 55% humidity
10:42:03.674Z  INFO display: 29.5 °C, 55% humidity
10:42:04.674Z  INFO display: 31.0 °C, 55% humidity
10:42:04.674Z  WARN display: too hot: 31.0 °C
```

Removing a branch also removes its wires, takes it off every `to` list, and
removes the arms nothing feeds any more.

**`cargo fmt`** tidies your code, and will move `// bonsai:input-arm` up
behind the last arm (`} // bonsai:input-arm`). That's fine: bonsai puts it
back on its own line the next time it adds an arm.

```sh
git add -A && git commit -m "Sensor, watchdog, display"
```

Next: [Order and tests](06-order-and-tests.md).

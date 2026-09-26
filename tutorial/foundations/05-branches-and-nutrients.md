# 5. Branches and nutrients

The greenhouse gets three parts:

- **sensor** reads the temperature every second.
- **watchdog** raises an alarm when it's too hot.
- **display** shows readings and alarms.

## Branch kinds

| command | the branch… | use it for |
|---------|-------------|------------|
| `bonsai branch <name>` | only receives | loggers, displays, actuators |
| `bonsai branch --produces <name>` | only sends | sensors, buttons, timers |
| `bonsai branch --duplex <name>` | receives *and* sends | logic that reacts: controllers, filters |

```sh
bonsai branch --produces sensor
bonsai branch display
bonsai branch --duplex watchdog
```

Each creates `src/branches/<name>.rs` and starts it from `src/main.rs`.

## Nutrients

A nutrient is a message type, with typed fields:

```sh
bonsai feed Reading temp_c10:i16 humidity:u8
bonsai feed Alarm temp_c10:i16
```

This adds `Reading { temp_c10: i16, humidity: u8 }` and
`Alarm { temp_c10: i16 }` to `Nutrient` in `src/trunk.rs`.

## Wiring

**release** = a branch sends a nutrient; **tap** = a branch receives it.

```sh
bonsai release sensor Reading
bonsai tap display Reading
bonsai tap watchdog Reading
bonsai release watchdog Alarm
bonsai tap display Alarm
bonsai list
```

```
sap: paths — one channel per nutrient
  Beat     broadcast  cap 2    pulse → pulse
  Reading  broadcast  cap 4    sensor → display, watchdog
  Alarm    broadcast  cap 4    watchdog → display
branches:
  display: taps Reading, Alarm
  sensor: releases Reading
  watchdog: taps Reading · releases Alarm
```

`bonsai` wrote the plumbing. Now fill in what each branch does: search for
`TODO`.

## Reading a scaffold

Open `src/branches/display.rs`. Every branch has the same two parts:

```rust
/// Launch this branch. The trunk calls this once at startup.
pub fn start(spawner: &Spawner, _trunk: &Trunk) {
    // Fails only if `run` is already running. A fixed message links no formatting code.
    spawner.spawn(
        run(sap::display::Taps::new())
            .unwrap_or_else(|_| panic!("display: run already running")),
    );
}

/// Receives the nutrients this branch taps, and only those.
#[embassy_executor::task]
async fn run(mut taps: sap::display::Taps) {
    loop {
        let nutrient: Nutrient = taps.next().await;
        #[allow(clippy::single_match, clippy::match_single_binding)] // until it taps more nutrients
        match nutrient {
            // tap nutrients with `bonsai tap display <Nutrient>`
            Nutrient::Reading { .. } => { /* TODO */ }
            Nutrient::Alarm { .. } => { /* TODO */ }
            // bonsai:nutrient-arm
            #[allow(unreachable_patterns)]
            _ => {}
        }
    }
}
```

`start` sets the branch up and `run` does its work. Your code goes in `run`.

| line | what it does | change it? |
|------|--------------|------------|
| `pub fn start(…)` | runs once at startup, called from `src/main.rs` | to hand the branch hardware (a pin, a port): add a parameter and pass it from `main.rs` |
| `spawner.spawn(run(…))` | starts `run` as a task | rarely. The panic can only happen if `run` is started twice |
| `#[embassy_executor::task]` | lets the executor run `run` alongside the other tasks | no |
| `taps.next().await` | waits for the next nutrient this branch taps | no |
| `Nutrient::Reading { .. } => …` | one arm per tapped nutrient | yes: this is your code |
| `// bonsai:…` marker lines | where `bonsai tap` and `bonsai release` add code | never delete them |
| `_ => {}` | ignores every other nutrient, so the branch compiles as you add more | no |
| `#[allow(…)]` | quiets lint warnings that only apply while there's one arm | you can delete them later, or leave them |

Branches that send start with a placeholder line, `let _ = &sap;`. It only
stops the compiler warning that `sap` is unused, so delete it once your code
calls `sap.release`.

## sensor

In `src/branches/sensor.rs`, add a timer import at the top:

```rust
use embassy_time::{Duration, Timer};
```

then make `run` wait a second, make up a reading, and release it. Replace
the `TODO` comment and the `core::future::pending` line (a placeholder that
waits forever), fill in the generated `Reading { /* TODO: fields */ }`, and
delete the `let _ = &sap;` placeholder:

```rust
async fn run(sap: Sap) {
    let mut tick: i16 = 0;
    loop {
        Timer::after(Duration::from_secs(1)).await;
        // A pretend sensor: 25.0 °C climbing towards 35 °C, then starting over.
        tick = (tick + 9) % 100;
        let temp_c10 = 250 + tick;
        let humidity = 55;
        sap.release(Nutrient::Reading { temp_c10, humidity }).await;
        // bonsai:emit
    }
}
```

## watchdog

`bonsai release` put `sap.release(Nutrient::Alarm { .. })` *after* the
`match`, where it would run for every nutrient. Delete it there (and the
`let _ = &sap;` placeholder), and send the alarm only when it's too hot,
inside the `Reading` arm:

```rust
async fn run(mut taps: sap::watchdog::Taps, sap: Sap) {
    let limit_c10: i16 = 300; // 30.0 °C
    loop {
        let nutrient: Nutrient = taps.next().await;
        #[allow(clippy::single_match, clippy::match_single_binding)] // until it taps more nutrients
        match nutrient {
            Nutrient::Reading { temp_c10, .. } => {
                if temp_c10 > limit_c10 {
                    sap.release(Nutrient::Alarm { temp_c10 }).await;
                }
            }
            // bonsai:nutrient-arm
            #[allow(unreachable_patterns)]
            _ => {}
        }
        // bonsai:emit
    }
}
```

`taps.next().await` waits for the next nutrient this branch taps, and only
those. The `_ => {}` arm ignores everything else, so the branch still
compiles as you add nutrients.

## display

Replace the two `{ /* TODO */ }` arms:

```rust
Nutrient::Reading { temp_c10, humidity } => {
    println!("{}.{} °C, {humidity}% humidity", temp_c10 / 10, temp_c10 % 10);
}
Nutrient::Alarm { temp_c10 } => {
    println!("ALARM: too hot ({}.{} °C)", temp_c10 / 10, temp_c10 % 10);
}
```

## Run it

```sh
cargo local
```

```
25.9 °C, 55% humidity
26.8 °C, 55% humidity
27.7 °C, 55% humidity
28.6 °C, 55% humidity
29.5 °C, 55% humidity
ALARM: too hot (30.4 °C)
30.4 °C, 55% humidity
```

(with `bonsai: beat` lines in between). Three independent tasks cooperate
through messages, and none of them knows the others exist.

## Changing your mind

Every command has an undo:

| do | undo |
|----|------|
| `bonsai branch …` | `bonsai snip <branch>` |
| `bonsai feed …` | `bonsai starve <Nutrient>` (also removes its wiring) |
| `bonsai tap …` | `bonsai untap <branch> <Nutrient>` |
| `bonsai release …` | `bonsai unrelease <branch> <Nutrient>` |

Removing wiring deletes the arm or release call, including any code you
wrote in it. Commit first.

Next: [Paths](06-paths.md).

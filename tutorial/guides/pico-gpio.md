# Pico GPIO: an LED and a button

Hand pins to branches: a button branch releases `Press`, and an LED branch
toggles on each one.

**Needs:** a Pico tree (plant one with `bonsai` → `pico` → `rp2040` →
`pico`), a button between GP15 and GND, and a debug probe. The on-board LED
is GP25.
**Crates:** none. `embassy-rp` is already in the tree.

## The rule: the trunk owns the hardware

`main.rs` takes the chip's peripherals once and hands each pin to exactly one
branch. The compiler enforces this: a pin can't be given away twice.

## Wire it

```sh
bonsai feed Press
bonsai branch --produces button
bonsai branch led
bonsai release button Press
bonsai tap led Press
```

## `src/branches/button.rs`

Take the pin in `start`, wait for presses instead of the `pending()` placeholder,
and delete the `let _ = &sap;` placeholder:

```rust
use embassy_rp::gpio::Input;

pub fn start(spawner: &Spawner, trunk: &Trunk, pin: Input<'static>) {
    spawner.spawn(run(trunk.sap(), pin).unwrap_or_else(|_| panic!("button: run already running")));
}

#[embassy_executor::task]
async fn run(sap: Sap, mut pin: Input<'static>) {
    loop {
        pin.wait_for_falling_edge().await;
        sap.release(Nutrient::Press).await;
        // bonsai:emit
    }
}
```

## `src/branches/led.rs`

```rust
use embassy_rp::gpio::Output;

pub fn start(spawner: &Spawner, _trunk: &Trunk, led: Output<'static>) {
    spawner.spawn(run(sap::led::Taps::new(), led).unwrap_or_else(|_| panic!("led: run already running")));
}

#[embassy_executor::task]
async fn run(mut taps: sap::led::Taps, mut led: Output<'static>) {
    loop {
        let nutrient: Nutrient = taps.next().await;
        #[allow(clippy::single_match, clippy::match_single_binding)]
        match nutrient {
            Nutrient::Press => led.toggle(),
            // bonsai:nutrient-arm
            #[allow(unreachable_patterns)]
            _ => {}
        }
    }
}
```

## `src/main.rs`: hand out the pins

```rust
use embassy_rp::gpio::{Input, Level, Output, Pull};
…
    let p = embassy_rp::init(Default::default());   // was `_p`
…
    branches::button::start(&spawner, &trunk, Input::new(p.PIN_15, Pull::Up));
    branches::led::start(&spawner, &trunk, Output::new(p.PIN_25, Level::Low));
```

## Run it

```sh
cargo run --release     # builds, flashes through the probe, streams logs
```

Each press toggles the LED. `wait_for_falling_edge().await` sleeps until the
pin changes, using no CPU while it waits.

## Notes

- **Debouncing:** a mechanical button bounces for a few milliseconds. For a
  clean press, sleep briefly after an edge
  (`Timer::after_millis(20).await`) before waiting for the next one.
- **Pico W / Pico 2 W:** the on-board LED is wired to the wireless chip, not
  GP25. Use an external LED on any GPIO instead.
- **Removing a branch:** `bonsai snip led` removes its start call, including
  the `Output::new(..)` you passed in. Any imports you added for it stay: delete
  them if nothing else uses them.

# Units

Numbers that carry their unit, so the compiler catches a temperature
compared with a distance, or meters mistaken for feet.

**Needs:** [chapter 5](../foundations/05-branches-and-messages.md).
**Crates:** none. The units are part of every tree's runtime
(`src/bonsai.rs`), in scope in `src/messages.rs` and every branch.

## What there is

Each unit is a struct around one `f32`: `Celsius(21.5)` makes one, `.0`
reads the number.

| kind | units | converts to |
|------|-------|-------------|
| temperature | `Celsius` °C, `Fahrenheit` °F, `Kelvin` K | each other |
| length | `Meters` m, `Feet` ft, `Kilometers` km | each other |
| speed | `MetersPerSecond` m/s, `Knots` kn, `KilometersPerHour` km/h | each other |
| time | `Seconds` s | `std::time::Duration`, both ways |
| frequency | `Hertz` Hz | |
| angle | `Degrees` °, `Radians` rad | each other; both have `.sin()`, `.cos()` |
| electrical | `Volts` V, `Amps` A, `Watts` W | |
| pressure | `Pascals` Pa, `Hectopascals` hPa | each other |
| share | `Percent` % | |

Every unit adds, subtracts and compares with the same unit (`+ - += -= <
>`), scales by a plain number (`Meters(2.0) * 3.0`), and divides by itself
into a plain ratio (`Meters(6.0) / Meters(2.0)` is `3.0`). It prints with
its symbol, and takes a precision: `{:.1}` of `Celsius(21.46)` is
`21.5 °C`.

## In messages

```sh
bonsai message add Reading temp:Celsius humidity:Percent
bonsai message add Fix altitude:Meters speed:Knots heading:Degrees
```

The `pub use crate::bonsai::units::*;` line at the top of
`src/messages.rs` brings them into scope there, and every branch's `use
crate::messages::*;` brings them into the branch. A tree planted before
units had that line gets it the first time `bonsai message add` uses a unit.
Unit names can't be message names.

## Converting

Within a kind, `.into()` or `From` converts:

```rust
let t = Celsius(21.5);
let f: Fahrenheit = t.into();                // 70.7 °F
let k = Kelvin::from(t);                     // 294.65 K
let alt = Feet::from(Meters(120.0));         // 394 ft
let wait: Duration = Seconds(1.5).into();    // 1.5 s, for tokio or std
```

Across kinds, the operations that make physical sense work, and give the
right unit:

```rust
let speed = Meters(120.0) / Seconds(8.0);    // MetersPerSecond(15.0)
let knots = Knots::from(speed);              // 29.2 kn
let far = speed * Seconds(2.0);              // Meters(30.0)
let power = Volts(5.0) * Amps(0.4);          // Watts(2.0)
let rate = 1.0 / Seconds(0.02);              // Hertz(50.0)
let x = Degrees(30.0).sin();                 // 0.5
```

Anything else is a compile error: `Meters(1.0) + Seconds(1.0)`,
`reading.temp > 30.0`, a `Feet` where a `Meters` is expected:

```
error[E0308]: mismatched types
  --> src/branches/watchdog.rs:28:35
   |
28 |                 if reading.temp > 30.0 {
   |                    ------------   ^^^^ expected `Celsius`, found floating-point number
```

## At the boundaries

Units live inside the tree. At its edges, choose a representation and
convert once:

- **Settings** in `bonsai.toml` are plain numbers: give them their unit in
  `setup` (`Celsius(settings::watchdog::LIMIT as f32)`), and name the key or
  its comment after the unit.
- **Text protocols** usually want the bare number: `reading.temp.0`.
- **Binary protocols** often use integers: tenths of a degree, centimeters.
  Convert where you encode and decode (see the
  [wire format guide](wire-format.md)).
- **An edge** can hand branches units directly: a custom edge's `In` can be
  `Celsius` ([custom edges](edges-custom.md)).

## Not here yet

A unit you need that isn't in the table is a plain struct away, in a file
of its own (bonsai reads every `pub struct` in `src/messages.rs` as a
message). In `src/lux.rs`, with `mod lux;` added to `src/main.rs`:

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd)]
pub struct Lux(pub f32);
```

and `use crate::lux::Lux;` at the top of `src/messages.rs`.

(bonsai's own units also get arithmetic and printing; ask for a unit to be
added to bonsai when it's common.)

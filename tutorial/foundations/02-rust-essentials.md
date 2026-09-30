# 2. Rust essentials

Just the Rust you need to read and write bonsai code, nothing more. Try any
snippet at <https://play.rust-lang.org>. When you want depth later, read
[the Rust Book](https://doc.rust-lang.org/book/).

## Values and types

```rust
let count = 3;               // a value; can't change
let mut total = 0;           // `mut`: can change
total += count;

let temp: f32 = 25.7;        // a 32-bit floating-point number
let humidity: u8 = 55;       // unsigned 8-bit integer: 0..=255
let hot: bool = temp > 30.0;
```

Rust has exact integer sizes (`u8`, `i16`, `u32`, `i64` …) and floating
point (`f32`, `f64`). `as` converts between number types:

```rust
let limit = 30.0_f64 as f32;         // a setting (f64) as an f32
let whole = 25.7_f32 as i16;         // 25: `as` cuts off, and doesn't check range
```

A number that measures something is clearer with its unit attached; bonsai
has types for that ([Units](#units), below).

## Printing and formatting

`format!` builds a `String`; `{}` is filled from the arguments, or from a
variable named inside the braces. `{:.1}` means one decimal place:

```rust
let text = format!("alarm {}", humidity);        // "alarm 55"
let text = format!("{temp:.1} °C");              // "25.7 °C"
```

`println!` prints the same way. In a bonsai tree you'll use `info!`,
`warn!` and friends instead (chapter 8): the same formatting, plus a time and
who wrote the line.

## Functions

```rust
fn too_hot(temp: f32, limit: f32) -> bool {
    temp > limit             // the last expression is the return value
}
```

## Conditions and loops

```rust
if temp > 35.0 {
    temp = 25.0;             // back to the start
} else {
    temp += 1.5;
}

for _ in 0..8 {              // eight times; `0..8` counts 0 to 7
    total += 1;
}
for temp in [26.5, 28.0, 29.5] {  // once for each value
    println!("{temp}");
}
```

`_` names a value you don't need, here the count.

## Structs

A struct is named fields together. Every message in a bonsai tree is one:

```rust
#[derive(Clone, Debug)]
pub struct Reading {
    pub temp: Celsius,
    pub humidity: Percent,
}

let r = Reading { temp: Celsius(25.7), humidity: Percent(55.0) };
println!("{}", r.temp);                     // 25.7 °C
```

`#[derive(Clone, Debug)]` asks the compiler to write two abilities for you:
`.clone()` makes a copy (one message can go to several branches), and
`{:?}` prints it (`Reading { temp: Celsius(25.7), humidity: Percent(55.0) }`),
which logs and tests use. `pub` makes a field visible outside its own file.

Some library structs have dozens of fields. When a type supports it, set the
ones you care about and let `..Default::default()` fill the rest with zeros
and empties:

```rust
let msg = ATTITUDE_DATA { roll, pitch, yaw, ..Default::default() };
```

(`roll` alone is shorthand for `roll: roll`.)

## Units

`Celsius` and `Percent` above are bonsai's **units**: a number with its unit
attached, so the compiler knows what it measures. Each is a struct with one
unnamed field, an `f32`: `Celsius(25.7)` makes one, and `.0` reads the
number back.

```rust
let mut temp = Celsius(25.0);
temp += Celsius(1.5);                        // 26.5 °C
let hot = temp > Celsius(30.0);              // compare like with like
println!("{temp:.1}");                       // 26.5 °C: it prints its symbol
let f: Fahrenheit = temp.into();             // 79.7 °F: `.into()` converts
let speed = Meters(120.0) / Seconds(4.0);    // MetersPerSecond(30.0)
```

Mixing up units is a compile error, not a bug found in the field. Comparing
a temperature with a bare number:

```
error[E0308]: mismatched types
  --> src/branches/watchdog.rs:28:35
   |
28 |                 if reading.temp > 30.0 {
   |                    ------------   ^^^^ expected `Celsius`, found floating-point number
```

Temperatures, lengths, speeds, time, frequency, angles, electrical and
pressure units are all there; the [units guide](../guides/units.md) lists
them.

## Methods and `impl`

Functions that belong to a type go in an `impl` block. `self` is the value
the method was called on; `&mut self` lets it change it:

```rust
pub struct Sensor {
    temp: Celsius,
}

impl Sensor {
    fn warm_up(&mut self) {
        self.temp += Celsius(1.5);
    }
}

let mut sensor = Sensor { temp: Celsius(25.0) };
sensor.warm_up();                    // now 26.5 °C
```

## Enums and `match`

An `enum` is one of several variants, and each variant can carry data. What
a branch receives is one: bonsai generates an `Input` enum for each branch,
with a variant for each thing wired to it:

```rust
enum Input {
    Tick,                    // no data: its clock ticked
    Reading(Reading),        // carries a Reading
    Alarm(Alarm),
}
```

`match` handles each variant and pulls the data out. It must cover every
variant, which is how the compiler tells you a branch forgot one:

```rust
match input {
    Input::Tick => println!("tick"),
    Input::Reading(reading) => println!("{}", reading.temp),
    Input::Alarm(_) => {}    // `_`: there's data, and I don't need it
}
```

An arm can have a condition (a *guard*) after `if`. It runs only when the
pattern fits *and* the condition holds:

```rust
match input {
    Input::Reading(reading) if reading.temp > Celsius(30.0) => println!("hot"),
    _ => {}                  // everything else
}
```

`if let` is a `match` with one interesting case. Chain more conditions with
`&&`, including more `let`s; the block runs only when all of them hold:

```rust
if let Input::Reading(reading) = input && reading.temp > Celsius(30.0) {
    println!("hot");
}
```

`matches!` asks "does this fit the pattern?" and gives a `bool`, and takes
a guard too. Tests use it a lot:

```rust
let hot = matches!(input, Input::Reading(r) if r.temp > Celsius(30.0));
let one_alarm = matches!(sent, [Msg::WatchdogAlarm(_)]);
```

`[a]` is a pattern for a list of exactly one thing, `[a, b]` of two.

## Traits

A *trait* is a set of methods that several types can have. bonsai's
`Branch` trait says what every branch has: a `setup` that makes its starting
state, and a `process` that handles one input. You write `impl Branch for
Sensor { … }` to give your `Sensor` those methods:

```rust
impl Branch for Sensor {
    type Input = Input;      // what it receives
    type Out = Out;          // where it sends

    fn setup() -> Self {     // `Self` = the type this is for: Sensor
        Sensor { temp: Celsius(25.0) }
    }

    fn process(&mut self, input: Input, out: &mut Out) {
        // …
    }
}
```

`type Input = Input;` fills in a blank the trait leaves (its *associated
types*). bonsai writes this part for you.

## Option and Result

There's no `null` in Rust. A value that might be missing is an `Option`
(`Some(value)` or `None`); an operation that might fail returns a `Result`
(`Ok(value)` or `Err(error)`):

```rust
let rest: Option<&str> = "limit 280".strip_prefix("limit ");  // Some("280")
match "280".parse::<i16>() {
    Ok(limit) => println!("got {limit}"),
    Err(e) => println!("bad input: {e}"),
}
```

`::<i16>` tells `parse` what to make. They combine naturally with `if let`:

```rust
if let Some(number) = text.strip_prefix("limit ")
    && let Ok(limit) = number.parse::<i16>()
{
    println!("new limit {limit}");
}
```

Inside a function that itself returns a `Result`, `?` means "if this failed,
return the error now":

```rust
fn open() -> std::io::Result<File> {
    let file = File::open("/dev/ttyS0")?;   // an Err returns from open() here
    Ok(file)
}
```

Avoid `.unwrap()`, which stops the program on a `None` or `Err`. Handle the
case instead: a bad packet from the network should be a warning, not a
crash.

## Text and bytes

`String` is text you own and can change; `&str` is a view of some text (a
`"literal"`, or part of a `String`). Bytes from a network or a serial port
are a `Vec<u8>`: a growable list of bytes.

```rust
let bytes: Vec<u8> = b"limit 280\n".to_vec();
let text = String::from_utf8_lossy(&bytes);   // bytes → text
let text = text.trim();                       // without the "\n"
```

A `Vec` of anything works the same way:

```rust
let mut temps = Vec::new();
temps.push(265);
temps.push(280);
assert_eq!(temps, [265, 280]);
```

## Ownership, in one paragraph

Every value has one owner. Passing it to a function *moves* it there, and
passing `&value` *lends* it (read-only; `&mut value` lends it for changing).
The compiler checks these rules, which is why Rust programs don't have
dangling pointers or data races. Most of the time you won't notice. When the
compiler complains, its error message usually says exactly what to change.

## Modules and `use`

Code is split into files (modules). `use` brings names into scope:

```rust
use crate::bonsai::Branch;           // `crate::` = this project
use crate::messages::*;              // `*`: everything in it
use crate::wiring::sensor::{Input, Out};
```

## Tests

A test is a function marked `#[test]`. It passes unless something in it
panics, which `assert!` (a condition) and `assert_eq!` (two equal values) do
when they don't hold. Tests go at the bottom of the file they test, in a
module only built for testing:

```rust
#[cfg(test)]
mod tests {
    use super::*;                    // everything in the file above

    #[test]
    fn too_hot_above_the_limit() {
        assert!(too_hot(31.0, 30.0));
        assert_eq!(too_hot(30.0, 30.0), false);
    }
}
```

## async and .await

A program that talks to the outside world waits a lot: for a packet, for
bytes on a serial port, for a connection. An `async fn` can pause at each
`.await` and let other code run until it can continue:

```rust
async fn next_packet(socket: &UdpSocket, buf: &mut [u8]) -> io::Result<usize> {
    let (n, _from) = socket.recv_from(buf).await?;   // pause here; others run
    Ok(n)
}
```

In a bonsai tree, only *edges* wait like this, and bonsai's built-in edges
already do it for you. Your branches never do. The next chapter explains
why.

## Crates

Libraries are *crates*, published on <https://crates.io>. Add one to your
project with:

```sh
cargo add serde --features derive
```

That edits `Cargo.toml`, your project's manifest.

Next: [How a tree runs](03-how-a-tree-runs.md).

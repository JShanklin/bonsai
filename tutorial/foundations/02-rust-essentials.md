# 2. Rust essentials

Just the Rust you need to read and write bonsai code, nothing more. Try any
snippet at <https://play.rust-lang.org>. When you want depth later, read
[the Rust Book](https://doc.rust-lang.org/book/).

## Values and types

```rust
let count = 3;               // a value; can't change
let mut total = 0;           // `mut`: can change
total += count;

let temp_c10: i16 = 257;     // signed 16-bit integer (257 = 25.7 °C)
let humidity: u8 = 55;       // unsigned 8-bit: 0..=255
let hot: bool = temp_c10 > 300;
```

Embedded code uses exact integer sizes (`u8`, `i16`, `u32` …) and avoids
floating point where it can. So bonsai's examples keep temperatures in tenths
of a degree.

## Functions

```rust
fn too_hot(temp_c10: i16, limit_c10: i16) -> bool {
    temp_c10 > limit_c10     // the last expression is the return value
}
```

## Enums and `match`

An `enum` is one of several variants, and each variant can carry data. Your
tree's messages, called *nutrients*, are one enum:

```rust
enum Nutrient {
    Beat,                                    // no data
    Reading { temp_c10: i16, humidity: u8 }, // named fields
    Alarm { temp_c10: i16 },
}
```

`match` handles each variant and pulls the data out:

```rust
match nutrient {
    Nutrient::Reading { temp_c10, humidity } => println!("{temp_c10} {humidity}"),
    Nutrient::Alarm { temp_c10 } => println!("too hot: {temp_c10}"),
    _ => {}                  // everything else: do nothing
}
```

`if let` is a `match` with one interesting case:

```rust
if let Nutrient::Alarm { temp_c10 } = nutrient {
    println!("too hot: {temp_c10}");
}
```

Add a condition with `if` on an arm (a *guard*) or `&&` after an `if let`.
The code runs only when the pattern fits *and* the condition holds:

```rust
match nutrient {
    Nutrient::Reading { temp_c10, .. } if temp_c10 > 300 => println!("hot"),
    _ => {}
}
if let Nutrient::Reading { temp_c10, .. } = nutrient && temp_c10 > 300 {
    println!("hot");
}
```

`..` in a pattern means "and the other fields, which I don't need".

## Structs

```rust
struct Reading {
    temp_c10: i16,
    humidity: u8,
}

let r = Reading { temp_c10: 257, humidity: 55 };
println!("{}", r.temp_c10);
```

Many library structs have dozens of fields. When a type supports it, set the
ones you care about and let `..Default::default()` fill the rest with zeros
and empties:

```rust
let msg = ATTITUDE_DATA { roll, pitch, yaw, ..Default::default() };
```

(`roll` alone is shorthand for `roll: roll`.)

## Option and Result

There's no `null` in Rust. A value that might be missing is an `Option`; an
operation that might fail returns a `Result`:

```rust
let n: Option<i16> = "315".parse().ok();   // Some(315), or None if not a number
match "315".parse::<i16>() {
    Ok(v) => println!("got {v}"),
    Err(e) => println!("bad input: {e}"),
}
```

Avoid `.unwrap()` in firmware. It stops the program on a bad value. Handle
the `None` or `Err` instead. The exception is startup on a Pi:
`.expect("can't open the port")` stops the program with that message, which
is the right call when the program can't work at all without the thing.

## Ownership, in one paragraph

Every value has one owner. Passing it to a function *moves* it there, and
passing `&value` *lends* it (read-only; `&mut value` lends it for changing).
The compiler checks these rules, which is why Rust programs don't have
dangling pointers or data races. Most of the time you won't notice. When the
compiler complains, its error message usually says exactly what to change.

## Closures and `move`

A *closure* is a function without a name, written inline. Its arguments go
between bars:

```rust
let double = |x: i32| x * 2;
println!("{}", double(21));                  // 42
```

Closures are how you hand a piece of work to someone else to run later, for
example to a thread. A closure can use variables from around it. `move` makes
it take ownership of them, which a thread needs because it can outlive the
function that started it:

```rust
let name = String::from("sensor");
std::thread::spawn(move || println!("hello from {name}"));
// `name` now belongs to the thread; this function can't use it any more
```

## Sharing with `Arc`

A value has one owner, so two threads can't both *own* one socket. `Arc` (an
"atomically reference-counted" pointer) solves that. `Arc::new` wraps the
value, and each `.clone()` makes another handle to the *same* value, not a
copy. The value is freed when the last handle goes.

```rust
use std::sync::Arc;

let socket = Arc::new(UdpSocket::bind("0.0.0.0:9001")?);
let sender = socket.clone();                  // same socket, second handle
std::thread::spawn(move || { /* receive on `socket` */ });
std::thread::spawn(move || { /* send on `sender` */ });
```

Every handle gets shared access only. That's enough for a socket, since its
send and receive work through a shared handle. When a thread must replace the
value, like a connection that drops and reopens, use `Arc<Mutex<…>>`:
`.lock()` gives one thread at a time full access.

## Modules and `use`

Code is split into files (modules). `use` brings names into scope:

```rust
use embassy_time::{Duration, Timer};
use crate::trunk::Nutrient;        // `crate::` = this project
```

## async and .await

Firmware waits a lot: for a timer, a button, bytes on a wire. An `async fn`
can pause at each `.await` and let other code run until it can continue:

```rust
async fn blink(mut led: Led) {
    loop {
        led.toggle();
        Timer::after(Duration::from_millis(500)).await; // pause here; others run
    }
}
```

That's the core idea behind how a bonsai tree runs, and it's the next
chapter.

## Crates

Libraries are *crates*, published on <https://crates.io>. Add one to your
project with:

```sh
cargo add serde --features derive
```

That edits `Cargo.toml`, your project's manifest.

Next: [How firmware runs](03-how-firmware-runs.md).

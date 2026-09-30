# Edges: your own

When UDP, TCP and serial don't fit (a file, a device driver, a library with
its own async API, a program's output), write an edge. It's one struct and
three functions, and bonsai supervises it like the built-in ones: its
failures and panics restart it with backoff, and its log lines carry its
name.

**Needs:** [chapter 7](../foundations/07-edges.md).

## The contract

```rust
impl MyEdge {
    pub async fn setup() -> io::Result<Self>;          // open it; an Err is retried
}

impl Edge for MyEdge {
    type In;                                           // what it receives
    type Out;                                          // what branches send it

    async fn recv(&mut self) -> io::Result<Self::In>;  // wait for the next thing in
    async fn execute(&mut self, out: Self::Out) -> io::Result<()>;  // carry one out
}
```

- **`setup`** opens whatever the edge keeps open. Returning `Err` (the `?`
  on a failed open does) makes bonsai wait and call it again: 0.1 s, then
  0.2 s, … up to 5 s apart, back to 0.1 s once it has run 30 s.
- **`recv`** waits for the next thing to hand the branches. An `Err`
  restarts the edge, `setup` and all.
- **`execute`** carries out one thing a branch sent. An `Err` restarts it
  too.
- **`In` and `Out`** can be any type that is `Clone + Debug + Send`: bytes,
  a number, your own struct. Branches wired from the edge get `In` as
  input; branches wired to it send `Out` with `out.to_<edge>(..)`.

**`recv` must be cancel-safe.** While it waits, a branch may send the edge
something, and bonsai then drops the `recv` in progress to `execute` it,
and calls `recv` again after. So `recv` must not lose data when it's dropped
at an `.await`: keep anything half-read in `self`, and wait with things
that are cancel-safe themselves (tokio's socket `recv`s, `Interval::tick`,
a channel's `recv`).

**Don't block.** An edge runs on the same thread as the core. A quick read
(a small file, a sysfs value) is fine; for anything slow, use the async
version (tokio's) or `tokio::task::spawn_blocking`.

## An example: the CPU's temperature

A Raspberry Pi reports its CPU temperature in a file. An edge that reads it
once a second gives the greenhouse a real sensor:

```sh
bonsai edge add cputemp --custom
bonsai branch add thermal
bonsai wire cputemp thermal
```

```
added edge cputemp: src/edges/cputemp.rs
wire it with `bonsai wire cputemp <branch>` (what it receives) and `bonsai wire <branch> cputemp` (what it sends)
updated src/wiring.rs, src/edges/mod.rs
…
cputemp --> thermal: what cputemp receives arrives as `Input::Cputemp(..)`
updated src/wiring.rs
```

A custom edge's keys in `bonsai.toml`, other than `kind`, are settings, like
a branch's. Put the file's path there:

```toml
[edge.cputemp]
kind = "custom"
path = "/sys/class/thermal/thermal_zone0/temp"
```

and run `bonsai sync`, which makes it `settings::cputemp::PATH`.

`src/edges/cputemp.rs`, filled in from the scaffold `bonsai edge add`
wrote:

```rust
use std::io;
use std::time::Duration;

use tokio::time::{Interval, interval};

use crate::bonsai::Edge;
use crate::settings::cputemp::PATH;

/// What cputemp keeps open: a timer, to read the temperature once a second.
pub struct Cputemp {
    every: Interval,
}

impl Cputemp {
    /// Setup: open it. An `Err` is retried with backoff.
    pub async fn setup() -> io::Result<Self> {
        // Read it once now, so a missing file fails here, and is retried.
        read(PATH)?;
        Ok(Cputemp {
            every: interval(Duration::from_secs(1)),
        })
    }
}

/// The temperature in tenths of a degree. The file holds thousandths.
fn read(path: &str) -> io::Result<i16> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| io::Error::new(e.kind(), format!("{path}: {e}")))?;
    let millis: i32 = text
        .trim()
        .parse()
        .map_err(|e| io::Error::other(format!("{path}: {e}")))?;
    Ok((millis / 100) as i16)
}

impl Edge for Cputemp {
    /// What it receives; the branches wired from it get it as input.
    type In = i16;
    /// What branches send it with `out.to_cputemp(..)`: nothing.
    type Out = ();

    /// The next thing it receives. Must be cancel-safe: keep partial data in self.
    async fn recv(&mut self) -> io::Result<Self::In> {
        self.every.tick().await;
        read(PATH)
    }

    /// Execute: carry out one thing a branch sent. An `Err` restarts the edge.
    async fn execute(&mut self, _out: Self::Out) -> io::Result<()> {
        Ok(())
    }
}
```

`.map_err(..)` turns one kind of error into another; here it puts the path
into the message, so the log says which file. `Interval::tick` is
cancel-safe, so dropping a `recv` mid-wait loses nothing. Nothing is ever
sent to this edge, so its `Out` is `()`, "nothing", and `execute` does
nothing.

The branch receives plain numbers:

```rust
Input::Cputemp(temp_c10) => {
    let temp = temp_c10 as f32 / 10.0;
    info!("CPU at {temp:.1} °C");
}
```

## Try it

On a computer without that file, point `path` at one you write yourself
(`path = "/tmp/cputemp"`, then `bonsai sync`), and start the tree before
the file exists:

```sh
cargo local
```

```sh
echo 48312 > /tmp/cputemp        # a moment later
echo 51060 > /tmp/cputemp
echo garbage > /tmp/cputemp
echo 49875 > /tmp/cputemp
```

```
11:01:05.762Z  INFO bonsai: running
11:01:05.762Z  WARN cputemp: /tmp/cputemp: No such file or directory (os error 2); retrying in 100ms
11:01:05.864Z  WARN cputemp: /tmp/cputemp: No such file or directory (os error 2); retrying in 200ms
11:01:06.066Z  WARN cputemp: /tmp/cputemp: No such file or directory (os error 2); retrying in 400ms
11:01:06.468Z  INFO cputemp: up
11:01:06.469Z  INFO thermal: CPU at 48.3 °C
11:01:07.469Z  INFO thermal: CPU at 48.3 °C
11:01:08.469Z  INFO thermal: CPU at 51.0 °C
11:01:09.470Z  WARN cputemp: /tmp/cputemp: invalid digit found in string; retrying in 800ms
11:01:10.271Z  WARN cputemp: /tmp/cputemp: invalid digit found in string; retrying in 1.6s
11:01:11.873Z  INFO cputemp: up
11:01:11.874Z  INFO thermal: CPU at 49.8 °C
```

A missing file, then a bad value: each failure restarts only the edge, and
the branch never sees a wrong number.

## Test it

A test drives the branch with the edge's `In`, no file needed:

```rust
core.handle(Event::Edge(EdgeIn::Cputemp(483)));
```

and `core.drain_cputemp()` returns what branches sent the edge.

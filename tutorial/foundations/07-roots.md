# 7. Roots

Until now the greenhouse has only talked to itself. A **root** is a branch
that connects the tree to the outside world: a network, a serial port,
another program. In this chapter the greenhouse sends its readings over the
network and takes a new temperature limit back.

Roots are for Pi / PC trees. On a microcontroller, drivers are already async,
so an ordinary branch talks to the hardware directly (see
[Pico GPIO](../guides/pico-gpio.md)).

## Why a root is different

Chapter 3's rule: [never block](03-how-firmware-runs.md#2-never-block). But
most I/O on Linux *does* block. `socket.recv()` sits and waits until a packet
arrives, and meanwhile every other task would stop.

A root keeps those blocking calls off the executor. They run on two OS
threads of their own, and messages pass between the threads and the tree
through two queues:

```
receive thread ──▶ inbox  ──▶ run ── sap.release ──▶ the tree
the tree ── taps ──▶ run ──▶ outbox ──▶ send thread
```

You write what each thread does with the link, and what `run` does with each
message. The threads and queues come from `src/roots.rs`, which bonsai writes
the first time you add a root. You never need to open it.

## Grow one

```sh
bonsai branch --roots remote
```

```
added roots branch `remote`:
  + src/branches/remote.rs
  + src/roots.rs (the bridge every root shares)
  ~ src/branches/mod.rs (module registered)
  ~ src/main.rs (started in the trunk)
  ~ Cargo.toml (embassy-futures added)
  ~ src/sap.rs (paths regenerated)
```

Wire it like any branch. It sends readings out and takes setpoints in:

```sh
bonsai tap remote Reading
bonsai release remote Setpoint
```

## Reading the scaffold

`src/branches/remote.rs` has the same `start` and `run` as every branch.

**`start`** opens the link and hands it to the bridge:

```rust
pub fn start(spawner: &Spawner, trunk: &Trunk) {
    // TODO: open your link here (a socket, a serial port, a protocol connection).
    let (inbox, outbox) = roots::bridge(
        "remote",
        // TODO: loop on the link's blocking receive and `deliver(msg)` each message.
        move |deliver| {
            let _ = deliver; // placeholder: delete once you deliver something
        },
        // TODO: write `msg` to the link. Blocking is fine: this runs on its own thread.
        move |msg: Outbound| drop(msg),
    );
    …
}
```

`roots::bridge` takes a name and two [closures](02-rust-essentials.md#closures-and-move).
The first runs once, on the receive thread: it loops forever, reading the
link and passing each message to `deliver`. The second runs on the send
thread, once for every message `run` puts in the outbox. Back come the
`inbox` and `outbox` that `run` uses.

**`run`** waits on both directions at once:

```rust
match select(inbox.next(), taps.next()).await {
    Either::First(msg) => {
        // TODO: decode `msg`, then release what it carries.
        …
    }
    Either::Second(nutrient) => {
        match nutrient {
            // TODO: encode each tapped nutrient, then `outbox.send(msg)`.
            Nutrient::Reading { .. } => { /* TODO */ }
            …
        }
    }
}
```

`select` waits for two things and returns whichever is ready first:
`Either::First` for a message from the link, `Either::Second` for a nutrient
from the tree. Nothing is lost when one wins, because the other waits for the
next time round the loop.

| TODO | what to change | why |
|------|----------------|-----|
| `Inbound` / `Outbound` | the message types, `Vec<u8>` to start | whatever is easiest to work with: text, bytes, or a protocol crate's message type |
| open your link | create the socket or port at the top of `start` | both threads need it, so it's made once, before them |
| the receive closure | loop on the blocking receive; `deliver(msg)` each message | runs on its own thread, so it may block as long as it likes |
| the send closure | write `msg` to the link | the same, for sending |
| `Either::First` | turn a message into a nutrient and `sap.release` it | the tree only speaks nutrients |
| `Either::Second` | turn each tapped nutrient into a message and `outbox.send` it | the link only speaks messages |

`deliver` waits if the tree falls 16 messages behind, which slows the
receive thread and never the tree. `outbox.send` never waits: if the link
falls 64 messages behind, the newest message is dropped (it returns `false`).

## Fill it in

The greenhouse talks plain text over UDP: it sends a line per reading, like
`25.9 C 55%`, and a message like `320` sets the limit to 32.0 °C. In
`src/branches/remote.rs`, add at the top:

```rust
use std::net::UdpSocket;
use std::sync::Arc;
```

Make both message types text, and name the addresses:

```rust
/// A line of text from the link, like "320".
pub type Inbound = String;
/// A line of text to the link, like "25.9 C 55%".
pub type Outbound = String;

const LISTEN: &str = "0.0.0.0:9001";
const SEND_TO: &str = "127.0.0.1:9000";
```

In `start`, open the socket and fill in both closures. Both threads use the
one socket, so it's shared with an [`Arc`](02-rust-essentials.md#sharing-with-arc):

```rust
    let socket = Arc::new(UdpSocket::bind(LISTEN).expect("remote: can't listen on 9001"));
    let sender = socket.clone();
    let (inbox, outbox) = roots::bridge(
        "remote",
        move |deliver| {
            let mut buf = [0u8; 64];
            loop {
                if let Ok(n) = socket.recv(&mut buf) {
                    deliver(String::from_utf8_lossy(&buf[..n]).into_owned());
                }
            }
        },
        move |msg: Outbound| {
            let _ = sender.send_to(msg.as_bytes(), SEND_TO);
        },
    );
```

`expect` is fine here: a root that can't open its port has nothing to do, so
the program stops and says why. The send result is ignored with `let _ =`
because UDP gives no delivery guarantee anyway.

In `run`, `bonsai release` added `sap.release(Nutrient::Setpoint { .. })` to
the `Either::First` arm. Make it parse the text first, and delete the `TODO`
lines and both placeholders (`let _ = &msg;` and `let _ = &sap;`):

```rust
            Either::First(msg) => {
                // "320" means a 32.0 °C limit. Anything else is ignored.
                if let Ok(temp_c10) = msg.trim().parse::<i16>() {
                    sap.release(Nutrient::Setpoint { temp_c10 }).await;
                }
                // bonsai:emit
            }
```

and fill in the `Reading` arm (then delete the `let _ = &outbox;` placeholder):

```rust
                    Nutrient::Reading { temp_c10, humidity } => {
                        let (whole, tenths) = (temp_c10 / 10, temp_c10 % 10);
                        outbox.send(format!("{whole}.{tenths} C {humidity}%\n"));
                    }
```

## Try it

Terminal 1, listen for readings:

```sh
nc -u -l 9000
```

Terminal 2:

```sh
cargo local
```

Terminal 1 shows every reading:

```
25.9 C 55%
26.8 C 55%
27.7 C 55%
```

Terminal 3, raise the limit to 40.0 °C:

```sh
printf '400' | nc -u -w0 127.0.0.1 9001
```

At 30.4 °C, where the watchdog used to raise an alarm, it now stays quiet
until the panel changes the limit again.

```
29.5 °C, 55% humidity
30.4 °C, 55% humidity
31.3 °C, 55% humidity
```

## You're done with the foundations

You can plant a tree, grow branches, define and wire nutrients, choose how
they flow, and connect the tree to the outside world. From here, pick
[guides](../README.md#guides-pick-what-you-need) for what your project needs:
binary wire formats, TCP, serial, MAVLink, hardware, tests, deployment.

# bonsai 🪴

> ⚠️ **Work in progress.** bonsai 2 is being built in steps (see
> [Status](#status)). Commands, templates and the generated code may still
> change.

**Grow a Linux application in Rust as a tree: a small trunk, and branches you
add one at a time.**

bonsai is a command-line tool for building Linux applications (a drone's
companion computer, a robot, a sensor hub) out of independent parts. You
describe the parts and how they're wired, and bonsai writes and maintains the
structure. You write what each part decides.

- **Branches** are the parts: a sensor, a controller, a radio link. Each keeps
  its own state, and turns each input into what it sends next.
- **Messages** are plain structs branches send each other. Branches never
  call each other, so any branch can be added, removed or rewritten without
  touching the rest.
- **Wires** say who sends what to whom. They live in `bonsai.toml`, one file
  for the whole graph, and bonsai generates the typed code that carries them.
- **Edges** are the bridges to the outside world: UDP (with multicast), TCP
  and serial ports, configured in `bonsai.toml` with no code, or your own.
  They do the I/O; branches only decide.
- **The core** runs every branch in one loop, one event at a time, on
  [tokio]. Everything a branch sends is delivered, in order, before the next
  event, so the same inputs always give the same outputs.

Trees run on Linux: a Raspberry Pi (Zero W, Zero 2 W, Pi 5) or the computer
you're on.

## Why

Programs that do several things at once tend to fail the same ways: a task
blocks and everything freezes; two parts race and the result depends on
timing; adding a feature means threading new plumbing through code that
already works. bonsai is built to prevent those:

- **Deterministic by design.** A branch's `process` does no I/O and never
  waits. The core feeds it one input at a time, in a fixed order. A test can
  drive a branch, or the whole core, with no runtime and no sockets.
- **The compiler checks the wiring.** Each branch gets an `Input` enum with
  exactly the messages wired to it. Wire in a new one and the build fails
  until the branch handles it; `out.send(m)` compiles only for messages the
  branch is wired to send.
- **One file is the graph.** Branches, their settings and rates, and every
  wire are in `bonsai.toml`, edited by commands or by hand. `bonsai list`
  shows it, and flags loops and branches nothing reaches.
- **A panic isn't the end.** A branch that panics is set up again; an edge
  that fails or panics is restarted with backoff. The rest of the tree keeps
  running, and the core never waits on an edge.
- **Small.** tokio with only the features the runtime uses, and no macros to
  compile: the glue is generated source.

## A look

```sh
bonsai                                   # plant a tree: pick a board, name it
bonsai branch add sensor
bonsai branch add display
bonsai message add Reading temp_c10:i16
bonsai message add Alarm temp_c10:i16
bonsai wire sensor Reading display
bonsai wire display Alarm sensor
bonsai rate sensor 4                     # an Input::Tick four times a second
bonsai edge add net udp --bind 0.0.0.0:6969 --to 10.0.0.2:6970
bonsai wire display net                  # display sends readings out: out.to_net(..)
bonsai list
```

```
tree: greenhouse  (host (native))
branches, in the order the core runs them:
  pulse  (ticks 2/s)
  sensor  (ticks 4/s)
  display
edges:
  net  (udp 0.0.0.0:6969 → 10.0.0.2:6970)
wires:
  sensor --Reading--> display
  display --Alarm--> sensor
  display --> net
warning: sensor → display → sensor send to each other in a loop: make at least one of those sends conditional
```

What you write is the part that matters:

```rust
fn process(&mut self, input: Input, out: &mut Out) {
    match input {
        Input::Reading(reading) => {
            if reading.temp_c10 > 250 {
                out.send(Alarm { temp_c10: reading.temp_c10 });
            }
        }
    }
}
```

## Learn it

**[The tutorial](tutorial/README.md)** goes from no background to building
real projects: eight chapters that grow one tree, **greenhouse**, from
planting it to watching it with `bonsai top`, then guides for each edge
(UDP, TCP, serial, MAVLink, your own), testing, deploying to a Pi, and CI.

## Install

Needs a recent Rust (`rustup update`) and [cargo-generate]:

```sh
cargo install cargo-generate
git clone https://github.com/JShanklin/bonsai.git && cd bonsai
cargo install --path .
```

The templates are built into the binary, so `bonsai` works from any directory.

The wizard asks for the board (a Raspberry Pi, or `host` for this computer),
then where to plant the tree. Then it offers
[build tools](tutorial/guides/build-tools.md) (sccache, mold, zigbuild, bacon),
installing any you pick that are missing.

## Commands

```
bonsai                                          plant a new tree (interactive)
bonsai init                                     plant it in the current folder
bonsai branch add|remove <name>                 add or remove a branch
bonsai message add <Name> [field:type …]        add a message type (remove undoes)
bonsai edge add <name> udp|tcp|serial [--key value …]   a bridge to the outside (remove undoes)
bonsai edge add <name> --custom                 an edge of your own, in src/edges/<name>.rs
bonsai wire <from> <Message> <to> [<to> …]      from sends it to each (unwire undoes)
bonsai wire <from> <to> [<to> …]                with an edge at one end: no message
bonsai rate <branch> <hz|off>                   tick a branch this many times a second
bonsai list                                     branches, wires, warnings
bonsai sync                                     regenerate the wiring after editing bonsai.toml
bonsai top [user@host] [--once]                 watch a running tree, here or on a Pi
bonsai update                                   refresh template crates and Cargo.lock
bonsai regrow                                   reset the tree to a fresh template
bonsai retarget <board>                         move the tree to another board
bonsai tools [<tool> …]                         build tools: sccache, mold, zigbuild, bacon
```

`bonsai branch` and `bonsai message` with no name ask for it interactively.

## Concepts

| term | is | in a tree |
|------|----|-----------|
| **tree** | an application project | the folder |
| **trunk** | startup: starts the core | `src/main.rs` |
| **branch** | a part: its state, and `setup` + `process` | `src/branches/<name>.rs` |
| **message** | what branches send each other | a struct in `src/messages.rs` |
| **edge** | a bridge to the outside: its I/O, restarted on failure | an `[edge.<name>]` in `bonsai.toml` |
| **wire** | `from` sends a message to branches in `to`, or an edge's packets in or out | a `[[wire]]` in `bonsai.toml` |
| **rate** | a branch's own clock: `Input::Tick`s per second | `rate` in its `[branch.<name>]` |
| **settings** | a branch's values, as constants | other keys in `[branch.<name>]` → `src/settings.rs` |
| **wiring** | the generated `Input`/`Out` types, the edges and the core | `src/wiring.rs` (never edit) |
| **pulse** | a built-in heartbeat, proof the tree is alive | `src/branches/pulse.rs` |

## Edges

```toml
[edge.tak]                    # bonsai edge add tak udp --bind … --to … --join …
kind = "udp"
bind = "0.0.0.0:6969"
to = "100.125.26.5:6970"      # where sends go (or `reply = true`: back to the last sender)
join = ["239.2.3.2"]          # multicast groups

[edge.fc]
kind = "tcp"
connect = "127.0.0.1:5760"    # a client that reconnects; `listen = "…"` for a server

[edge.gps]
kind = "serial"
device = "/dev/serial0"
baud = 9600
framing = "lines"             # a packet per line; "raw" (the default) passes each read on
```

Built-in edges carry `Packet { bytes, peer }`: `peer` is who sent it, and on
the way out who gets it (`packet.reply(bytes)` answers the sender). A branch
wired from an edge gets `Input::Tak(packet)`; one wired to it sends with
`out.to_tak(packet)`. Decoding (MAVLink, a protobuf) belongs in `process`, so
it stays testable. For anything else, `bonsai edge add <name> --custom`
scaffolds an `Edge` with typed `In`/`Out`, `setup`, `recv` and `execute`.

A test can drive the whole core without sockets:

```rust
let mut core = Core::new();
core.handle(Event::Edge(EdgeIn::Net(Packet::new("hi"))));
assert_eq!(core.drain_net(), [Packet::new("HI")]);
```

## Logs

Branches and edges log with `info!`, `warn!`, `error!` and `debug!`, which
work like `println!`. Each line says when (UTC), how serious, and who wrote
it, with nothing to pass in:

```
14:05:03.123Z  INFO bonsai: running
14:05:03.124Z  INFO tak: up
14:05:03.125Z  INFO pulse: beat
14:05:04.310Z ERROR sensor: panicked at src/branches/sensor.rs:31: index out of bounds
14:05:04.310Z  WARN sensor: set up again after a panic
14:05:05.002Z  WARN fc: connect 127.0.0.1:5760: Connection refused (os error 111); retrying in 100ms
```

`BONSAI_LOG` picks what's shown: a level (`off`, `error`, `warn`, `info`,
the default, or `debug`), then any branch or edge that should differ:
`BONSAI_LOG=warn,sensor=debug`. Lines go to stderr, so a systemd service's
land in the journal, and `cargo test` hides them unless a test fails. A panic
is one line; set `RUST_BACKTRACE=1` for the backtrace too.

## Watching a tree

`bonsai top`, run in a tree's folder while it runs, shows it live: every
branch in the order the core runs them (inputs and sends per second, time
per input, panics), every edge (up or retrying, packets in and out, drops,
restarts, the last error), and the log. `↑`/`↓` and `enter` show one
branch's or edge's lines only. Here, a tree echoing UDP packets back
uppercase, just after one packet made it panic:

```
 logtree  up 4s  8 events/s  slowest event 125.0 ms  0 waiting
┌ branches ────────────────────────────────────────────────────────────────┐
│branch                      inputs/s    sent/s    avg µs    max µs  panics│
│pulse                            2.0       0.0        21        32       0│
│echo                             6.0       6.0         7        12       1│
└──────────────────────────────────────────────────────────────────────────┘
┌ edges ───────────────────────────────────────────────────────────────────┐
│edge          state          in/s     out/s   dropped  restarts last error│
│net           up              6.0       6.0         0         0           │
│radio         up              0.0       0.0         0         0           │
└──────────────────────────────────────────────────────────────────────────┘
┌ log ─────────────────────────────────────────────────────────────────────┐
│06:54:05.088Z  INFO pulse: beat                                           │
│06:54:05.588Z  INFO pulse: beat                                           │
│06:54:05.995Z ERROR echo: panicked at src/branches/echo.rs:30: asked to   │
│06:54:06.120Z  WARN echo: set up again after a panic                      │
│06:54:06.120Z  INFO pulse: beat                                           │
│06:54:06.587Z  INFO pulse: beat                                           │
│06:54:07.087Z  INFO pulse: beat                                           │
│06:54:07.588Z  INFO pulse: beat                                           │
│06:54:08.088Z  INFO pulse: beat                                           │
│06:54:08.588Z  INFO pulse: beat                                           │
└──────────────────────────────────────────────────────────────────────────┘
 ↑↓ select  enter show only its log  p pause  q quit
```

The tree serves these on `127.0.0.1:7777`, on its own computer only. For a
tree that builds for a Pi, `bonsai top` goes there over ssh (`BONSAI_PI`, as
`cargo run` does; `ssh -W`, so the Pi needs nothing but sshd); `bonsai top
user@host` names one, `bonsai top local` this computer. `--once` prints the
tables and exits. `BONSAI_TOP` moves a tree's server to another port
(`BONSAI_TOP=7778`; `bonsai top --port 7778`) or turns it off (`off`).
Counting what branches do never changes what they send.

## Status

bonsai 2 lands in steps:

1. ✅ Linux only: Raspberry Pi boards and `host`.
2. ✅ The deterministic core: branches, messages, wires, rates, settings.
3. ✅ Edges: built-in UDP (with multicast), TCP (client and server) and
   serial, configured in `bonsai.toml`, plus an `Edge` trait for your own.
4. ✅ Logs tagged with the branch or edge that wrote them.
5. ✅ Stats, and `bonsai top`: a live view of a running tree.
6. ✅ The tutorial, rewritten.

| board | chip | target | status |
|-------|------|--------|--------|
| `zero-w` | BCM2835 | `arm-unknown-linux-gnueabihf` (ARMv6) | ✅ builds (zigbuild) |
| `zero-2w` | BCM2710A1 | `aarch64-unknown-linux-gnu` | ✅ builds |
| `pi5` | BCM2712 | `aarch64-unknown-linux-gnu` | ✅ builds |
| `host` | this computer | the computer's own | ✅ builds and runs |

**Trees from bonsai 1** (Embassy, `src/sap.rs`, nutrients) aren't supported:
the commands refuse them. Plant a new tree and move each branch's logic into a
`process`. `bonsai regrow` resets one to a fresh bonsai 2 tree, which deletes
its branches.

## Contributing

- **The code:** `src/main.rs` is the CLI (the wizard, `tools`, `regrow`,
  `retarget`, `update`); `src/tree.rs` holds the commands that grow a tree;
  `src/graph.rs` reads `bonsai.toml` and generates the wiring. It's pure and
  unit-tested. Per-board templates live in `templates/linux/<board>/` and are
  rendered by cargo-generate; the branch scaffold and the runtime
  (`templates/_branch/`, `templates/_tree/`) are built into the binary.
- **Adding a board:** add it to `BOARDS` in `src/main.rs`, and add
  `templates/linux/<board>/` (copy a similar board). See
  [templates/README.md](templates/README.md).
- **Checks:** `cargo fmt --check`, `cargo clippy --all-targets`,
  `cargo test`.

[tokio]: https://tokio.rs/
[cargo-generate]: https://cargo-generate.github.io/cargo-generate/

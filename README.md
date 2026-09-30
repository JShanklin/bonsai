# bonsai 🪴

**Grow a Linux application in Rust as a tree: a small trunk, and branches you
add one at a time.**

bonsai is a command-line tool for building Linux applications (a drone's
companion computer, a robot, a sensor hub) out of independent parts. You
describe the parts and how they're linked, and bonsai writes and maintains the
structure. You write what each part decides.

- **Branches** are the parts: a sensor, a controller, a radio link. Each keeps
  its own state, and turns each input into what it sends next.
- **Messages** are plain structs branches send each other. Branches never
  call each other, so any branch can be added, removed or rewritten without
  touching the rest.
- **Links** say who sends what to whom. They live in `bonsai.toml`, one file
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
- **The compiler checks the links.** Each branch gets an `Input` enum with
  exactly the messages linked to it. Link in a new one and the build fails
  until the branch handles it; `out.send(m)` compiles only for messages the
  branch is linked to send.
- **One file is the graph.** Branches, their settings and rates, and every
  link are in `bonsai.toml`, edited by commands or by hand. `bonsai list`
  shows it, and flags loops and branches nothing reaches.
- **A panic isn't the end.** A branch that panics is set up again; if its
  setup panics too, it's out of service (its inputs dropped and counted)
  and tried again with backoff. An edge that fails or panics is restarted
  with backoff. The rest of the tree keeps running, and the core never
  waits on an edge.
- **A record of every run.** Each run gets a folder named by when it
  started, with a file per kind of line: the events you mark with
  `record!("launch: altitude {alt}")`, panics, errors, edges coming and
  going. Every file starts with START and ends with END and why; a run with
  no END didn't shut down cleanly. Old runs are pruned (`keep_runs`,
  `keep_days`), and long runs' files roll over (`max_file_kb`).
- **Units the compiler checks.** `temp: Celsius`, `alt: Meters`, `speed:
  Knots`: comparing a temperature with a bare number, or adding meters to
  seconds, doesn't build. `Meters / Seconds` is `MetersPerSecond`, and each
  prints with its symbol.
- **Small.** tokio with only the features the runtime uses, and no macros to
  compile: the glue is generated source.

## A look

```sh
bonsai                                   # plant a tree: pick a board, name it
bonsai branch add sensor
bonsai branch add display
bonsai message add Reading temp:Celsius
bonsai message add Alarm temp:Celsius
bonsai link sensor Reading display
bonsai link display Alarm sensor
bonsai rate sensor 4                     # an Input::Tick four times a second
bonsai edge add net udp --bind 0.0.0.0:6969 --to 10.0.0.2:6970
bonsai link display net                  # display sends readings out: out.to_net(..)
bonsai list
```

```
tree: greenhouse  (host (native))
branches, in the order the core runs them:
  sensor  (ticks 4/s)
  display
edges:
  net  (udp 0.0.0.0:6969 → 10.0.0.2:6970)
links:
  sensor --Reading--> display
  display --Alarm--> sensor
  display --> net
record: events, panics → logs/
warning: sensor → display → sensor send to each other in a loop: make at least one of those sends conditional
```

What you write is the part that matters:

```rust
fn process(&mut self, input: Input, out: &mut Out) {
    match input {
        Input::Reading(reading) => {
            if reading.temp > Celsius(25.0) {
                out.send(Alarm { temp: reading.temp });
            }
        }
    }
}
```

## Learn it

**[The tutorial](tutorial/README.md)** goes from no background to building
real projects: eight chapters that grow one tree, **greenhouse**, from
planting it to watching it with `bonsai top`, then guides for each edge
(UDP, TCP, serial, your own), testing, deploying to a Pi, and CI.

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
bonsai link <from> <Message> <to> [<to> …]      from sends it to each (unlink undoes)
bonsai link <from> <to> [<to> …]                with an edge at one end: no message
bonsai rate <branch> <hz|off>                   tick a branch this many times a second
bonsai list                                     branches, links, warnings
bonsai sync                                     regenerate src/links.rs after editing bonsai.toml
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
| **link** | `from` sends a message to branches in `to`, or an edge's packets in or out | a `[[link]]` in `bonsai.toml` |
| **rate** | a branch's own clock: `Input::Tick`s per second | `rate` in its `[branch.<name>]` |
| **settings** | a branch's values, as constants | other keys in `[branch.<name>]` → `src/settings.rs` |
| **units** | numbers with their unit: `Celsius`, `Meters`, `Knots`… | in every tree's runtime, used in messages |
| **`src/links.rs`** | the generated `Input`/`Out` types, the edges and the core | written by bonsai (never edit) |

## Edges

```toml
[edge.net]                    # bonsai edge add net udp --bind … --to … --join …
kind = "udp"
bind = "0.0.0.0:6969"
to = "100.125.26.5:6970"      # where sends go (or `reply = true`: back to the last sender)
join = ["239.2.3.2"]          # multicast groups

[edge.hub]
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
linked from an edge gets `Input::Net(packet)`; one linked to it sends with
`out.to_net(packet)`. Decoding (a binary format, a protobuf) belongs in `process`, so
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
10:44:38.343Z  INFO bonsai: running
10:44:38.343Z  INFO uplink: up
10:44:38.344Z  INFO display: 26.5 °C, 55% humidity
…
10:44:43.344Z  WARN display: too hot: 34.0 °C
10:44:43.344Z ERROR display: panicked at src/branches/display.rs:35: way too hot
10:44:43.344Z  WARN display: set up again after a panic
```

`BONSAI_LOG` picks what's shown: a level (`off`, `error`, `warn`, `info`,
the default, or `debug`), then any branch or edge that should differ:
`BONSAI_LOG=warn,sensor=debug`. Lines go to stderr, so a systemd service's
land in the journal, and `cargo test` hides them unless a test fails. A panic
is one line; set `RUST_BACKTRACE=1` for the backtrace too.

## Watching a tree

`bonsai top`, run in a tree's folder while it runs, shows it live, in five
tabs you switch with their number, `Tab` or a click: the **graph** (every
branch and edge as a box coloured by its state: green busy, grey idle, red
after a panic or while retrying; every link an arrow with its message and
rate), **branches** (inputs and sends a second, time per input, panics),
**edges** (up or retrying, packets in and out, drops, restarts, the last
error), the **log** (scroll, search, filter by level or by who wrote it)
and the **system** (the tree's CPU and memory, the computer's load). Here,
the tutorial's greenhouse, its display selected just after a panic:

```
 greenhouse  up 8s  1 events/s  slowest event 265 µs  0 waiting
  1 Graph   2 Branches   3 Edges   4 Log   5 System
┌ graph ─────────────────────────────────────────────────────────────────────────────────┐
│             ┌────────────────────────────────────────────┐                             │
│             │                                            │                             │
│┌──────────┐ │Reading 1.0/s   ┌──────────┐  Alarm 0.0/s   │ ┌─────────┐                 │
││ sensor   │─┴─┬─────────────▶│ watchdog │────────────────┴▶│ display │                 │
││ 1.0/s    │   │              │ 1.0/s    │                  │ 1.0/s   │                 │
│└──────────┘   │              └──────────┘                  └─────────┘                 │
│               │                    │                                                   │
│╭──────────╮   │0.0/s               │                                                   │
││ uplink   │───┘                    │                                                   │
││ up 0.0/s │                        │                                                   │
│╰──────────╯                        │                                                   │
│       ▲                            │                                                   │
│       └─ 0.0/s ────────────────────┘                                                   │
│                                                                                        │
│                                                                                        │
│                                                                                        │
│                                                                                        │
│────────────────────────────────────────────────────────────────────────────────────────│
│ display: 1.0 inputs/s, avg 24 µs, max 61 µs, 1 panic                                   │
│12:19:09.095Z  WARN display: set up again after a panic                                 │
│12:19:10.094Z  INFO display: 25.0 °C, 55% humidity                                      │
│12:19:11.095Z  INFO display: 26.5 °C, 55% humidity                                      │
│12:19:12.095Z  INFO display: 28.0 °C, 55% humidity                                      │
└────────────────────────────────────────────────────────────────────────────────────────┘
 1-5/tab/click switch  ←→↑↓ select  enter its log  p pause  q quit
```

The tree serves these on `127.0.0.1:7777`, on its own computer only. For a
tree that builds for a Pi, `bonsai top` goes there over ssh (`BONSAI_PI`, as
`cargo run` does; `ssh -W`, so the Pi needs nothing but sshd); `bonsai top
user@host` names one, `bonsai top local` this computer. `--once` prints the
tables and exits. `BONSAI_TOP` moves a tree's server to another port
(`BONSAI_TOP=7778`; `bonsai top --port 7778`) or turns it off (`off`).
Counting what branches do never changes what they send.

## Boards

| board | chip | target | status |
|-------|------|--------|--------|
| `zero-w` | BCM2835 | `arm-unknown-linux-gnueabihf` (ARMv6) | ✅ builds (zigbuild) |
| `zero-2w` | BCM2710A1 | `aarch64-unknown-linux-gnu` | ✅ builds |
| `pi5` | BCM2712 | `aarch64-unknown-linux-gnu` | ✅ builds |
| `host` | this computer | the computer's own | ✅ builds and runs |

**Coming from the old bonsai** (Embassy, `src/sap.rs`, nutrients)? Its
trees aren't supported: the commands refuse them. Plant a new tree and move
each branch's logic into a `process`. `bonsai regrow` resets an old tree to a
fresh one, which deletes its branches.

## Contributing

- **The code:** `src/main.rs` is the CLI (the wizard, `tools`, `regrow`,
  `retarget`, `update`); `src/tree.rs` holds the commands that grow a tree;
  `src/graph.rs` reads `bonsai.toml` and generates `src/links.rs`. It's pure and
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

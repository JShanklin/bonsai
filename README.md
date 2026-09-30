# bonsai 🪴

> ⚠️ **Work in progress.** Commands, templates and the generated code may
> still change, and not every board is equally tested. See [Status](#status).

**Grow a Linux application in Rust as a tree: a small trunk, and branches you
add one at a time.**

bonsai is a command-line tool for building Linux applications (a drone's
companion computer, a robot, a sensor hub) out of independent parts.
You describe the parts and how they talk, and bonsai writes and maintains the
structure. You write what each part does.

- **Branches** are subsystems: a sensor, a controller, a radio link. Each runs
  as its own async task, and they all share one CPU core cooperatively.
- **Nutrients** are the messages branches exchange. Branches never call each
  other. They *release* nutrients and *tap* the ones they care about, so any
  branch can be added, removed or rewritten without touching the rest.
- **The sap** is the plumbing between them: one channel per nutrient, generated
  from your wiring and regenerated every time it changes. You never edit it.

Trees run on Linux: a Raspberry Pi (Zero W, Zero 2 W, Pi 5) or the computer
you're on, on top of [Embassy]'s async executor.

## Why

Programs that do several things at once tend to fail the same ways. A task
blocks and everything freezes. One busy data stream crowds out a critical
command. Two parts wait on each other forever. Queues get sized by guesswork.
And adding a feature means threading new plumbing through code that already
works.

bonsai is built to prevent those:

- **Structure, not boilerplate.** Branch scaffolds, message types, channel
  wiring and subscriber counts are generated. What's specific to your
  application (the sensor read, the protocol, the decision) is left as a
  clearly marked `TODO`.
- **Every message flow is explicit.** Each nutrient has a *shape*: broadcast
  (events), directed (commands that must not drop), or state (the latest
  value). It also has a capacity. `bonsai list` shows the whole flow graph.
- **Hazards are caught at wiring time.** Self-deadlocks, directed cycles,
  feedback loops and messages that reach only one of their readers are reported
  the moment you create them, not in the field.
- **Loss is never silent.** A reader that falls behind loses the oldest
  messages, and every loss is counted and reported.
- **Small by default.** Each channel is sized for its own message, and release
  profiles are tuned for the Pi (size-optimised, stripped, LTO).

## A look

```sh
bonsai                                  # plant a tree: pick a board, name it
                                        # (`bonsai init` plants it in this folder)
bonsai branch --produces sensor         # a branch that sends
bonsai branch --duplex watchdog         # a branch that receives and sends
bonsai branch display                   # a branch that receives
bonsai feed Reading temp_c10:i16 humidity:u8
bonsai feed Alarm --directed temp_c10:i16
bonsai release sensor Reading
bonsai tap watchdog Reading
bonsai release watchdog Alarm
bonsai tap display Reading
bonsai tap display Alarm
bonsai list
```

```
sap: paths — one channel per nutrient
  Beat     broadcast  cap 2    pulse → pulse
  Reading  broadcast  cap 4    sensor → display, watchdog
  Alarm    directed   cap 8    watchdog → display
branches:
  display: taps Reading, Alarm
  sensor: releases Reading
  watchdog: taps Reading · releases Alarm
```

What you write is the part that matters:

```rust
Nutrient::Reading { temp_c10, .. } => {
    if temp_c10 > limit_c10 {
        sap.release(Nutrient::Alarm { temp_c10 }).await;
    }
}
```

## Learn it

**[The tutorial](tutorial/README.md)** goes from no background to building
real projects. Its foundations need no hardware: the first project runs on
your computer. Short focused guides then cover what real devices need:
UDP, TCP, serial and MAVLink links, wire formats, GPIO, testing, deployment
and CI.

## Install

Needs a recent Rust (`rustup update`) and [cargo-generate]:

```sh
cargo install cargo-generate
git clone https://github.com/JShanklin/bonsai.git && cd bonsai
cargo install --path .
```

The templates are built into the binary, so `bonsai` works from any directory.

The wizard asks for the board (a Raspberry Pi, or `host` for this computer),
then where to plant the tree. Then it offers [build tools](tutorial/guides/build-tools.md) (sccache, mold,
zigbuild, bacon), installing any you pick that are missing.

## Commands

```
bonsai                                          plant a new tree (interactive)
bonsai init                                     plant it in the current folder
bonsai branch [--produces|--duplex|--roots] [<name>]   add a branch   (snip undoes)
bonsai feed [<Name> [--broadcast|--directed|--state] [--cap N] [field:type …]]
                                                add a nutrient  (starve undoes)
bonsai tap <branch> <Nutrient>                  branch receives it  (untap undoes)
bonsai release <branch> <Nutrient>              branch sends it     (unrelease undoes)
bonsai path <Nutrient> [<shape>] [--cap N]      show or reshape a nutrient's path
bonsai list                                     flow graph, branches, warnings
bonsai sync                                     regenerate the sap after hand edits
bonsai update                                   refresh template crates and Cargo.lock
bonsai regrow                                   reset the tree to a fresh template
bonsai retarget <board>                         move the tree to another board
bonsai tools [<tool> …]                         build tools: sccache, mold, zigbuild, bacon
```

Branch kinds: none (receives), `--produces` (sends), `--duplex` (both),
`--roots` (bridges blocking I/O such as sockets and serial ports into the
tree).

## Concepts

| term | is | in a tree |
|------|----|-----------|
| **tree** | an application project | the folder |
| **trunk** | startup: hands out devices, starts every branch | `src/main.rs`, `src/trunk.rs` |
| **branch** | a subsystem (one or more tasks) | `src/branches/<name>.rs` |
| **nutrient** | a message type | a `Nutrient` variant in `src/trunk.rs` |
| **sap** | the generated channels | `src/sap.rs` (never edit) |
| **path** | one nutrient's channel: shape + capacity | `bonsai.toml` |
| **pulse** | a built-in heartbeat, proof the tree is alive | `src/pulse.rs` |

| shape | flow | when full | for |
|-------|------|-----------|-----|
| **broadcast** (default) | one → many | a slow reader loses the oldest (counted) | readings, events, streams |
| **directed** | many → one | the sender waits; nothing lost | commands, work queues, TX |
| **state** | latest value | overwritten; late readers still get it | setpoints, modes, link status |

Paths are generated per nutrient, and each queue holds only its own message
type. For tiny trees, `layout = "trunk"` in `bonsai.toml` puts everything on
one shared bus instead.

## Status

| board | chip | target | status |
|-------|------|--------|--------|
| `zero-w` | BCM2835 | `arm-unknown-linux-gnueabihf` (ARMv6) | ✅ builds (zigbuild); runs on a virtual Pi |
| `zero-2w` | BCM2710A1 | `aarch64-unknown-linux-gnu` | ✅ builds and runs on a host |
| `pi5` | BCM2712 | `aarch64-unknown-linux-gnu` | ✅ builds and runs on a host |
| `host` | this computer | the computer's own | ✅ builds and runs |

Microcontrollers (Pico, ESP32) were supported by earlier versions and have been
dropped: bonsai is a Linux tool now. Not there yet: validation on every Pi, and
a crates.io release (install from source for now).

**Older trees.** Trees grown before per-nutrient paths (no `src/sap.rs`) are no
longer supported. `bonsai regrow` makes a fresh one. To keep your branches,
copy `bonsai.toml`, `src/sap.rs`, `src/pulse.rs` and the non-`Nutrient` parts of
`src/trunk.rs` from a fresh tree of the same board, and add `use trunk::sap;` to
`src/main.rs`. Then in each branch, replace subscribers with
`sap::<branch>::Taps::new()` and `taps.next().await`, and
`sap.publish_immediate(x)` with `sap.release(x).await`. Finish with `bonsai sync`.

## Contributing

- **The code:** `src/main.rs` is the CLI (commands, TUIs, file edits), and
  `src/flow.rs` generates the sap from the wiring graph. It's pure and
  unit-tested. Per-board templates live in `templates/linux/<board>/` and are
  rendered by cargo-generate; branch scaffolds in `templates/_branch/` are
  built into the binary.
- **Adding a board:** add it to `BOARDS` in `src/main.rs`, and add
  `templates/linux/<board>/` (copy a similar board;
  `trunk.rs`, `bonsai.toml` and `branches/` are portable). Then run
  `bonsai sync` inside the new template to generate its `src/sap.rs`. See
  [templates/README.md](templates/README.md).
- **Checks:** `cargo fmt --check`, `cargo clippy --all-targets`,
  `cargo test`.

[Embassy]: https://embassy.dev/
[cargo-generate]: https://cargo-generate.github.io/cargo-generate/

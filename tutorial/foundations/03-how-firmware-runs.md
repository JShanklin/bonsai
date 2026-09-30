# 3. How firmware runs

Five ideas explain every bonsai tree.

## 1. One program, many tasks

A device does several things at once: read a sensor, drive a display, talk on
a radio. Each becomes a **task**, a function that loops forever. The
**executor** (bonsai uses [Embassy](https://embassy.dev)) runs them all on one
CPU core by switching between them at every `.await`.

```
sensor task:  ──read──.await(1 s)─────────────read──.await(1 s)───…
display task: ────────────.await(msg)──print──────────.await(msg)─…
```

This is **cooperative**. A task only gives up the CPU at an `.await`.

## 2. Never block

Because of that, a task must never sit in a loop, or in a blocking call,
without an `.await`. While it does, *every other task stops*: your heartbeat,
your display, everything.

- Waiting? Use an async wait: `Timer::after(..).await`, a driver's
  `read().await`, a pin's `wait_for_falling_edge().await`.
- Stuck with a blocking library (a Linux socket, a serial port)? Run it on an
  OS thread. That's what bonsai's `--roots` branches are for (chapter 7).

## 3. Tasks talk by message, never directly

Tasks don't call each other or share variables. They pass messages. bonsai
calls these **nutrients** and generates the channels they flow through (the
**sap**). A task *releases* a nutrient, and any task that *taps* it receives
it. That keeps each part independent: add, remove or rewrite one without
touching the others.

## 4. On Linux

A bonsai tree is an ordinary Linux program, on a Raspberry Pi or on your own
computer:

| | a bonsai tree |
|---|---|
| Rust mode | `std`: the full standard library (files, threads, the network) |
| hardware access | Linux devices (`/dev/…`) and crates |
| running it | `cargo run`; on a Pi, copied over ssh and run there |
| logging | `println!` / `eprintln!` |

The same tree runs on every board. Only the build target, and how it gets to
the Pi, differ.

## 5. The tree

bonsai names the parts of your program after a tree:

| term | what it is | where it lives |
|------|-----------|----------------|
| **tree** | your project | the whole folder |
| **trunk** | startup: hands out devices, starts every branch | `src/main.rs`, `src/trunk.rs` |
| **branch** | one subsystem, one or more tasks | `src/branches/<name>.rs` |
| **nutrient** | a message type | a variant of `Nutrient` in `src/trunk.rs` |
| **sap** | the generated channels nutrients flow through | `src/sap.rs` (never edit it) |
| **path** | one nutrient's channel, with its shape and capacity | `bonsai.toml` |
| **pulse** | a built-in heartbeat proving the tree is alive | `src/pulse.rs` |

The `bonsai` command does the wiring for you: it creates branches and
nutrients, connects them, and regenerates the sap. You write what each branch
*does*.

Next: [Your first tree](04-first-tree.md).

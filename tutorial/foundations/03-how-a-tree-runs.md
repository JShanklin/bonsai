# 3. How a tree runs

Six ideas explain every bonsai tree.

## 1. Parts that decide, and parts that talk

A program on a companion computer or a sensor hub does several things: read
a sensor, decide whether it's too hot, tell a ground station. bonsai splits
it into two kinds of part:

- **Branches** decide. Each keeps its own state and turns each input into
  what it sends next. A branch never touches the network, a file or a
  clock.
- **Edges** talk to the outside world: a UDP socket, a TCP connection, a
  serial port. What an edge receives goes to the branches wired to it, and
  what branches send it, it carries out.

## 2. One event at a time

Everything that happens to a tree is an **event**: a branch's clock ticked
(its **rate**), or an edge received something. One loop, the **core**,
takes events one at a time, in the order they came:

```
events:  tick(sensor)   packet(uplink)   tick(sensor)   …
            │                 │                │
core:    sensor ─Reading─▶ watchdog ─Alarm─▶ display    (all of it, then the next event)
```

The core hands the event to the branches wired to it, then delivers every
message they send, and every message *those* send, until nothing is left.
Only then does it take the next event. This is **run to completion**.

## 3. `setup` and `process`

A branch is a struct with two functions:

- `setup` makes its starting state.
- `process` takes one input and sends what it decides with `out.send(..)`.

`process` does no I/O and never waits. So the same inputs always give the
same outputs, in the same order. That's what **deterministic** means here,
and it's why a test can drive a branch, or the whole tree, with no network
and no timers: hand it inputs, check what it sent.

## 4. Messages and wires

Branches never call each other. They send **messages**: plain structs like
`Reading { temp_c10, humidity }`. A **wire** says who sends which message to
whom: `sensor` sends `Reading` to `watchdog` and `display`. Wires live in
`bonsai.toml`, one file for the whole tree, and bonsai generates the typed
code that carries them.

The compiler checks the wiring. Each branch gets an `Input` enum with exactly
what's wired to it, so a new wire doesn't build until the branch handles it.
`out.send(m)` builds only for messages the branch is wired to send.

## 5. Edges run on their own

An edge waits for the outside world in its own task on
[tokio](https://tokio.rs), Rust's most used runtime for programs that wait
on the network. The core never waits for an edge: what a branch sends one is
queued, and if the edge falls behind, the extra is dropped and counted
rather than stalling the tree.

When an edge fails (the port is in use, the connection drops, a bug makes
it panic), bonsai starts it again after a short wait that grows with each
failure: 0.1 s, 0.2 s, … up to 5 s. The rest of the tree carries on.

## 6. A panic isn't the end

If a branch's `process` panics (a failed `assert!`, a bad index), the core
logs it, runs that branch's `setup` again, and goes on with the next event.
The other branches keep their state.

## The tree

bonsai names the parts of your program after a tree:

| term | is | in a tree |
|------|----|-----------|
| **tree** | an application project | the folder |
| **trunk** | startup: starts the core | `src/main.rs` |
| **branch** | a part: its state, and `setup` + `process` | `src/branches/<name>.rs` |
| **message** | what branches send each other | a struct in `src/messages.rs` |
| **edge** | a bridge to the outside: its I/O, restarted on failure | an `[edge.<name>]` in `bonsai.toml` |
| **wire** | who sends what to whom | a `[[wire]]` in `bonsai.toml` |
| **rate** | a branch's own clock: `Input::Tick`s per second | `rate` in its `[branch.<name>]` |
| **settings** | a branch's values, as constants | other keys in `[branch.<name>]` → `src/settings.rs` |
| **wiring** | the generated `Input`/`Out` types, the edges and the core | `src/wiring.rs` (never edit) |

The `bonsai` command does the plumbing: it adds branches, messages and
edges, wires them, and regenerates the wiring. You write what each branch
decides.

Next: [Your first tree](04-first-tree.md).

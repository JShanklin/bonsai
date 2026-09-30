# 8. Watching a tree

A tree that runs on a Pi in a box needs to tell you what it's doing. This
chapter covers its logs, what a panic looks like, and `bonsai top`, a live
view of a running tree.

## Logs

You've been using them since chapter 5. `error!`, `warn!`, `info!` and
`debug!` work like `println!`, from any branch or edge, and each line says
when (UTC), how serious, and who wrote it:

```
11:55:44.028Z  INFO watchdog: limit is now 28.0 °C
```

Nothing to pass in: bonsai knows which branch is running, or which edge.
Lines go to stderr, so a service's lines land in its journal
([deploy guide](../guides/deploy.md)), and `cargo local-test` hides them
unless a test fails.

**Pick what's shown** with `BONSAI_LOG`: a level (`off`, `error`, `warn`,
`info`, the default, or `debug`), then any branch or edge that should
differ. Only warnings and errors:

```sh
BONSAI_LOG=warn cargo local
```

```
11:56:05.123Z  WARN display: too hot: 31.0 °C
11:56:06.124Z  WARN display: too hot: 32.5 °C
11:56:07.124Z  WARN display: too hot: 34.0 °C
```

Only warnings, except the uplink, which tells everything:

```sh
BONSAI_LOG=warn,uplink=info cargo local
```

**Each run also leaves a folder** in `logs/`, named by the local time it
started (`logs/2026-09-30_20-07-45/`). It holds `events.log`, with what
branches mark with `record!("…")` (like `info!`, kept), and `panics.log`,
each starting with a START line and ending with an END line that says why
the tree stopped. `[record]` in `bonsai.toml` picks what's kept: see the
[run logs guide](../guides/run-logs.md).

```
11:56:07.751Z  INFO uplink: up
11:56:10.752Z  WARN display: too hot: 31.0 °C
```

`debug!` lines are hidden unless asked for, so leave them in:
`BONSAI_LOG=watchdog=debug` shows them for the watchdog alone. On a
terminal the level is coloured; set `NO_COLOR=1` to turn that off.

## A panic

Make the display panic when it's much too hot, to see what happens. In its
`Input::Alarm` arm, make the `warn!` a block and add an `assert!` after it:

```rust
Input::Alarm(alarm) => {
    warn!("too hot: {:.1}", alarm.temp);
    assert!(alarm.temp < Celsius(33.0), "way too hot");
}
```

```
11:56:28.626Z  INFO display: 32.5 °C, 55% humidity
11:56:28.626Z  WARN display: too hot: 32.5 °C
11:56:29.626Z  INFO display: 34.0 °C, 55% humidity
11:56:29.626Z  WARN display: too hot: 34.0 °C
11:56:29.626Z ERROR display: panicked at src/branches/display.rs:33: way too hot
11:56:29.626Z  WARN display: set up again after a panic
11:56:30.626Z  INFO display: 25.0 °C, 55% humidity
11:56:31.625Z  INFO display: 26.5 °C, 55% humidity
```

One line says where it panicked, and the display was set up again: its
`setup` ran, and it went on with the next reading. The sensor and the
watchdog never noticed. For the full backtrace, run with
`RUST_BACKTRACE=1`. Keep the `assert!` for the next section, then delete it.

If `setup` itself panics (at startup, or when setting up again), the branch
is **out of service**: `ERROR display: setup panicked: out of service, its
inputs dropped; trying again in 1s`. Its inputs are dropped and counted,
everything else carries on, and `setup` is tried again on an input a
second later, then after 2 s, 4 s… up to a minute, never in a tight loop.
`bonsai top` shows the branch red, `(out of service)`, until a `setup`
works: `INFO display: set up again: back in service`.

## bonsai top

While the tree runs, open another terminal in the tree's folder:

```sh
bonsai top local
```

It opens on the tree's graph. Here the arrow keys have selected the display,
just after its panic:

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

Every branch is a square box and every edge a round one, laid out left to
right the way messages flow, each with its inputs (or packets) a second.
Colour shows state: green when busy, grey when idle, red for a branch that
panicked in the last 10 seconds (the display, here) or an edge that's
retrying, yellow for an edge starting up. Arrows are the links, labelled
with the message and how many a second, and brighter while busy; a link
that closes a loop (the watchdog answering the uplink) runs back underneath.
The strip at the bottom is the selected node: its numbers and latest lines.

The top row holds five **tabs**; switch with their number, `Tab`, or a
click:

1. **Graph**, above. `←→↑↓` select a node; `enter` opens its log.
2. **Branches**: each branch in the order the core runs them, with inputs
   and sends a second, the average and longest time `process` took, and
   panics.

   ```
   ┌ branches ──────────────────────────────────────────────────────────────────────────────┐
   │branch                                    inputs/s    sent/s    avg µs    max µs  panics│
   │sensor                                         1.0       1.0         4         6       0│
   │watchdog                                       1.0       2.0         3         7       0│
   │display                                        2.0       0.0        19        32       1│
   │                                                                                        │
   ```

3. **Edges**: `up` or `retrying`, packets in and out a second, what was
   **dropped** because the edge's queue was full, what was **lost** after
   the edge took it (it failed carrying it out, or couldn't deliver it to a
   slow client), restarts, and why it last failed.
4. **Log**: the lines as they come. `↑↓`/`PgUp`/`PgDn` scroll back, `/`
   searches, `l` steps through the levels shown (all, info+, warn+,
   errors), `esc` clears.
5. **System**: the tree's CPU and memory, its threads, and the computer's
   memory and load.

**The header** above the tabs is the core, on every tab: how long it's run,
events a second, the slowest event so far, and how many are waiting (more
than a few means the core is falling behind). `p` pauses the numbers; `q`
quits. It's all one program in one terminal: no tmux or zellij needed.

Why `local`? The greenhouse is a Pi tree, so plain `bonsai top` goes to the
Pi in `.cargo/config.toml`'s `BONSAI_PI`, over ssh, as `cargo run` does. It
needs nothing on the Pi but ssh. Without a Pi yet:

```
$ bonsai top --once
bonsai top: ssh: Could not resolve hostname raspberrypi.local: Name or service not known
```

`--once` prints the numbers instead of the live view, for scripts, the
links included:

```sh
bonsai top local --once
```

```
greenhouse: up 9s, 1 events/s, slowest event 143 µs, 0 waiting
branch            inputs/s    sent/s    avg µs    max µs  panics
sensor                 1.0       1.0         4         5       0
watchdog               1.0       0.0         2         7       0
display                1.0       0.0        21        32       1
edge                 state      in/s     out/s   dropped      lost restarts  last error
uplink                  up       0.0       0.0         0         0       0  
link                                                msgs/s
sensor --Reading--> watchdog, display                  1.0
watchdog --Alarm--> display                            0.0
uplink --> watchdog                                    0.0
watchdog --> uplink                                    0.0
```

The tree serves these on `127.0.0.1:7777`, on its own computer only (ssh is
what lets you in from elsewhere). Two trees on one computer need different
ports: `BONSAI_TOP=7778 cargo local`, then `bonsai top local --port 7778`.
`BONSAI_TOP=off` turns it off.

Counting what branches do never changes what they send: the tree behaves
the same with `bonsai top` watching or not.

Now delete the `assert!` from the display, and commit.

## Where next

The greenhouse is done: three branches, two messages, an edge, tests, and a
way to watch it. From here, pick a guide from the
[tutorial's index](../README.md#guides-pick-what-you-need): put it
[on a Pi](../guides/deploy.md), add a [serial](../guides/edges-serial.md)
sensor, or write [your own edge](../guides/edges-custom.md).

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

## bonsai top

While the tree runs, open another terminal in the tree's folder:

```sh
bonsai top local
```

```
 greenhouse  up 12s  1 events/s  slowest event 327 µs  0 waiting
┌ branches ──────────────────────────────────────────────────────────────────┐
│branch                        inputs/s    sent/s    avg µs    max µs  panics│
│sensor                             1.0       1.0         6        25       0│
│watchdog                           1.0       2.0         3        10       0│
│display                            2.0       0.0        20        42       2│
└────────────────────────────────────────────────────────────────────────────┘
┌ edges ─────────────────────────────────────────────────────────────────────┐
│edge            state          in/s     out/s   dropped  restarts last error│
│uplink          up              0.0       1.0         0         0           │
└────────────────────────────────────────────────────────────────────────────┘
┌ log ───────────────────────────────────────────────────────────────────────┐
│11:56:43.352Z  INFO display: 31.0 °C, 55% humidity                          │
│11:56:43.352Z  WARN display: too hot: 31.0 °C                               │
│11:56:44.352Z  INFO display: 32.5 °C, 55% humidity                          │
│11:56:44.352Z  WARN display: too hot: 32.5 °C                               │
│11:56:45.352Z  INFO display: 34.0 °C, 55% humidity                          │
│11:56:45.352Z  WARN display: too hot: 34.0 °C                               │
│11:56:45.352Z ERROR display: panicked at src/branches/display.rs:33: way too│
│11:56:45.352Z  WARN display: set up again after a panic                     │
└────────────────────────────────────────────────────────────────────────────┘
 ↑↓ select  enter show only its log  p pause  q quit
```

- **The header** is the core: how long it's run, events per second, the
  slowest event so far, and how many events are waiting (more than a few
  means the core is falling behind).
- **Branches**, in the order the core runs them: inputs and sends per
  second, the average and longest time `process` took, and panics.
- **Edges**: `up` or `retrying`, packets in and out per second, what was
  dropped because the edge fell behind, how often it restarted, and why it
  last failed.
- **The log**: the latest lines. `↑`/`↓` pick a branch or edge, and `enter`
  shows only its lines (`esc` shows them all again). `p` pauses the tables;
  `q` quits.

Why `local`? The greenhouse is a Pi tree, so plain `bonsai top` goes to the
Pi in `.cargo/config.toml`'s `BONSAI_PI`, over ssh, as `cargo run` does. It
needs nothing on the Pi but ssh. Without a Pi yet:

```
$ bonsai top --once
bonsai top: ssh: Could not resolve hostname raspberrypi.local: Name or service not known
```

`--once` prints the tables instead of the live view, for scripts:

```sh
bonsai top local --once
```

```
greenhouse: up 9s, 1 events/s, slowest event 327 µs, 0 waiting
branch            inputs/s    sent/s    avg µs    max µs  panics
sensor                 1.0       1.0         6        25       0
watchdog               1.0       0.0         2         9       0
display                1.0       0.0        23        42       1
edge                 state      in/s     out/s   dropped restarts  last error
uplink                  up       0.0       0.0         0       0  
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
[on a Pi](../guides/deploy.md), talk [MAVLink](../guides/edges-mavlink.md),
or write [your own edge](../guides/edges-custom.md).

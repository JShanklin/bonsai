# 6. Paths

Every nutrient flows along its own **path**, a channel bonsai generates.
You choose each path's **shape** and **capacity**. That's the difference
between a system that holds up under load and one that stalls or loses data.

## Three shapes

| shape | flag | behaves like | when it's full | use it for |
|-------|------|--------------|----------------|------------|
| broadcast *(default)* | `--broadcast` | one sender → every tapper gets a copy | a slow tapper loses the oldest (and it's counted) | readings, events, streams |
| directed | `--directed` | many senders → **one** receiver | the sender waits; nothing is lost | commands, work queues, outgoing frames |
| state | `--state` | the latest value | overwritten; a late tapper still gets the current value | setpoints, modes, link up/down |

Rules of thumb:
- **Losing is worse than waiting** → directed.
- **Waiting is worse than losing** → broadcast.
- **Only now matters** → state.

**Capacity** (`--cap N`) is how many nutrients a path queues. Size it for the
biggest burst between two reads. It doesn't apply to state paths.

## Shape the greenhouse

```sh
bonsai path Reading --cap 8          # two readers; give a slow one some slack
bonsai path Alarm directed --cap 4   # an alarm must never be dropped
```

Both are recorded in `bonsai.toml`, and bonsai regenerates `src/sap.rs`.
Your branch code doesn't change.

## A state path: the setpoint

Right now the 30.0 °C limit is hard-coded. Make it a setting that a control
panel can change:

```sh
bonsai branch --produces panel
bonsai feed Setpoint --state temp_c10:i16
bonsai release panel Setpoint
bonsai tap watchdog Setpoint
bonsai list
```

```
  Beat      broadcast  cap 2    pulse → pulse
  Reading   broadcast  cap 8    sensor → display, watchdog
  Alarm     directed   cap 4    watchdog → display
  Setpoint  state      latest   panel → watchdog
```

`src/branches/panel.rs`: pretend someone flips the limit every 10 seconds.
Add the timer import as before (`use embassy_time::{Duration, Timer};`),
then fill in `run` the same way as the sensor's:

```rust
async fn run(sap: Sap) {
    let mut high = true;
    loop {
        Timer::after(Duration::from_secs(10)).await;
        let temp_c10 = if high { 320 } else { 300 };
        high = !high;
        sap.release(Nutrient::Setpoint { temp_c10 }).await;
        // bonsai:emit
    }
}
```

`src/branches/watchdog.rs`: make the limit a variable, and update it from the
new arm:

```rust
    let mut limit_c10: i16 = 300;
    …
            Nutrient::Setpoint { temp_c10 } => limit_c10 = temp_c10,
```

Because `Setpoint` is a state path, a branch added later still gets the
current limit the moment it starts.

## bonsai warns you

bonsai checks the wiring every time you change it. Try a mistake: let the
watchdog tap its own alarms.

```sh
bonsai tap watchdog Alarm
```

```
warning: `Alarm` is directed but tapped by display and watchdog — each nutrient
         reaches only one of them. Make it broadcast if they should all see it.
warning: deadlock risk: `watchdog` taps and releases directed `Alarm`.
         `sap.release(..).await` waits while that path is full — and the only task
         that drains it is `watchdog` itself. Use `sap.try_release(..)` there, or make
         `Alarm` broadcast.
```

Both are real bugs. Undo it:

```sh
bonsai untap watchdog Alarm
```

| warning | means | fix |
|---------|-------|-----|
| directed … tapped by A and B | each message reaches only one of them | make it broadcast |
| deadlock risk | a branch waits for room only it can make | `sap.try_release(..)`, or broadcast |
| … taps and releases … feeds itself | a loop that never ends | release only under a condition |

## Watching the paths

A broadcast tapper that falls behind loses the oldest nutrients, and bonsai
counts every loss. Run with lag reports:

```sh
BONSAI_SAP_DEBUG=1 cargo local    # Pi/PC trees; on a Pico set DEFMT_LOG = "debug"
```

```
sap: display lagged on Reading, 3 lost
```

To see how full each path is, log `sap::depths()` from any branch. It returns
`(nutrient, queued, capacity)` for every path. If a path is *always* full,
make its reader faster rather than raising the cap.

## One shared bus, for tiny trees

In `bonsai.toml`, `layout = "trunk"` puts every nutrient on a single bus. It
uses the least RAM, but every tapper wakes for every nutrient, and one flood
crowds out the rest. Keep the default `"paths"` unless RAM is truly tight.

Next: [Roots](07-roots.md).

# 7. Edges

Until now the greenhouse has only talked to itself. An **edge** connects it
to the outside world: a network, a serial port, another program. In this
chapter the greenhouse sends its alarms over the network and takes a new
temperature limit back.

## Why an edge is separate

A branch's `process` never waits (chapter 3). But talking to the outside
world is mostly waiting: for a packet, for a connection, for a slow port.
An edge does that waiting in its own task, and hands the core what it
receives as an event, one at a time like any other. What a branch sends an
edge is queued for it; the core never waits for it to go out.

bonsai has three edges built in, configured in `bonsai.toml` with no code:
**UDP** (with multicast), **TCP** (client or server) and **serial**. They
all carry a `Packet`: the bytes, and `peer`, who sent them (or, on the way
out, who they're for). For anything else you can write your own
([custom edges](../guides/edges-custom.md)).

## Add one

```sh
bonsai edge add uplink udp --bind 0.0.0.0:6969 --to 127.0.0.1:6970
```

```
added udp edge uplink
wire it with `bonsai wire uplink <branch>` (what it receives) and `bonsai wire <branch> uplink` (what it sends)
updated src/wiring.rs
warning: edge uplink isn't wired: `bonsai wire uplink <branch>` to hear it, `bonsai wire <branch> uplink` to send
```

It listens on port 6969, and sends to port 6970 on this computer (in real
life, the address of whatever should hear the alarms). In `bonsai.toml`:

```toml
[edge.uplink]
kind = "udp"
bind = "0.0.0.0:6969"
to = "127.0.0.1:6970"
```

## Wire it

A wire with an edge at one end carries its packets, so it takes no message
name:

```sh
bonsai wire uplink watchdog
bonsai wire watchdog uplink
bonsai list
```

```
uplink --> watchdog: what uplink receives arrives as `Input::Uplink(..)`
updated src/wiring.rs
watchdog --> uplink: send from watchdog with `out.to_uplink(..)`
updated src/wiring.rs
tree: greenhouse  (zero-2w (bcm2710a1))
branches, in the order the core runs them:
  sensor  (ticks 1/s)
  watchdog  (settings: limit_c10)
  display
edges:
  uplink  (udp 0.0.0.0:6969 → 127.0.0.1:6970)
wires:
  sensor --Reading--> watchdog, display
  watchdog --Alarm--> display
  uplink --> watchdog
  watchdog --> uplink
```

The watchdog gained an `Input::Uplink(_uplink) => {}` arm, and an
`out.to_uplink(packet)` to send with.

## Decide what the bytes mean

The edge moves bytes; the watchdog decides what they mean. That keeps the
decoding in `process`, where a test can reach it. Plain text is enough to
start: the watchdog sends `alarm 310` when it's too hot, and takes `limit
280` to set a new limit, answering the sender. (For compact binary
messages, see the [wire format guide](../guides/wire-format.md).)

In `src/branches/watchdog.rs`, bring `Packet` in:

```rust
use crate::bonsai::{Branch, Packet};
```

and fill in `process`:

```rust
fn process(&mut self, input: Input, out: &mut Out) {
    match input {
        // `bonsai wire <from> <Message> watchdog` adds an arm here
        Input::Reading(reading) => {
            if reading.temp_c10 > self.limit_c10 {
                out.send(Alarm {
                    temp_c10: reading.temp_c10,
                });
                let text = format!("alarm {}\n", reading.temp_c10);
                out.to_uplink(Packet::new(text));
            }
        }
        Input::Uplink(packet) => {
            // `limit 280` sets a new limit, in tenths of a degree.
            let bytes = String::from_utf8_lossy(&packet.bytes);
            let text = bytes.trim();
            if let Some(number) = text.strip_prefix("limit ")
                && let Ok(limit) = number.parse::<i16>()
            {
                self.limit_c10 = limit;
                info!("limit is now {limit}");
                out.to_uplink(packet.reply(format!("ok, limit {limit}\n")));
            } else {
                warn!("didn't understand {text:?}");
            }
        }
        // bonsai:input-arm
    }
}
```

`Packet::new(text)` goes to the edge's `to` address. `packet.reply(..)`
goes back to whoever sent `packet`. A packet the watchdog doesn't
understand gets a warning, not a crash: it came from outside, so expect
anything.

## Try it

You need three terminals, and `socat` (`sudo apt install socat`, or `brew
install socat`), a tool that sends and receives on sockets. First, the tree:

```sh
cargo local
```

Second, listen where the alarms go:

```sh
socat -u UDP-RECV:6970 STDOUT
```

Third, send the tree a new limit once the alarms are coming in, then
something it won't understand:

```sh
echo "limit 280" | socat - UDP:127.0.0.1:6969
echo "hello" | socat - UDP:127.0.0.1:6969
```

```
ok, limit 280
```

The listener shows the alarms: three at the old limit, then one at 29.5 °C
under the new one:

```
alarm 310
alarm 325
alarm 340
alarm 295
```

And the tree's log:

```
10:43:48.340Z  INFO bonsai: running
10:43:48.341Z  INFO uplink: up
10:43:48.342Z  INFO display: 26.5 °C, 55% humidity
…
10:43:53.343Z  INFO display: 34.0 °C, 55% humidity
10:43:53.343Z  WARN display: too hot: 34.0 °C
10:43:53.796Z  INFO watchdog: limit is now 280
10:43:54.303Z  WARN watchdog: didn't understand "hello"
10:43:54.342Z  INFO display: 25.0 °C, 55% humidity
10:43:55.343Z  INFO display: 26.5 °C, 55% humidity
10:43:56.342Z  INFO display: 28.0 °C, 55% humidity
10:43:57.342Z  INFO display: 29.5 °C, 55% humidity
10:43:57.342Z  WARN display: too hot: 29.5 °C
```

## When an edge fails

Start the tree while something else holds port 6969, and the edge can't
open. It tries again, waiting longer each time, while the rest of the tree
carries on. Once the port is free, it comes up:

```
10:44:05.947Z  INFO bonsai: running
10:44:05.948Z  WARN uplink: bind 0.0.0.0:6969: Address already in use (os error 98); retrying in 100ms
10:44:05.949Z  INFO display: 26.5 °C, 55% humidity
10:44:06.049Z  WARN uplink: bind 0.0.0.0:6969: Address already in use (os error 98); retrying in 200ms
10:44:06.250Z  WARN uplink: bind 0.0.0.0:6969: Address already in use (os error 98); retrying in 400ms
10:44:06.651Z  WARN uplink: bind 0.0.0.0:6969: Address already in use (os error 98); retrying in 800ms
10:44:06.948Z  INFO display: 28.0 °C, 55% humidity
10:44:07.452Z  WARN uplink: bind 0.0.0.0:6969: Address already in use (os error 98); retrying in 1.6s
10:44:07.949Z  INFO display: 29.5 °C, 55% humidity
10:44:08.949Z  INFO display: 31.0 °C, 55% humidity
10:44:08.949Z  WARN display: too hot: 31.0 °C
10:44:09.054Z  INFO uplink: up
```

The same happens when a TCP connection drops or a serial port is unplugged.

## Tests

Run the tests again and one fails:

```sh
cargo local-test
```

```
---- branches::watchdog::tests::alarms_only_above_the_limit stdout ----

thread 'branches::watchdog::tests::alarms_only_above_the_limit' (6702) panicked at src/branches/watchdog.rs:81:9:
assertion failed: matches!(out.sent(), [Msg::WatchdogAlarm(Alarm { temp_c10: 310 })])
```

Good: the watchdog now sends two things above the limit, and the test says
so. Update what it expects:

```rust
        assert!(matches!(
            out.sent(),
            [
                Msg::WatchdogAlarm(Alarm { temp_c10: 310 }),
                Msg::WatchdogToUplink(_)
            ]
        ));
```

A test can also drive the whole tree, edges included, with no sockets:
`core.handle(..)` takes one event, as the running tree would, and
`drain_uplink()` returns everything sent to the edge. Add this to the bottom
of `src/main.rs`:

```rust
#[cfg(test)]
mod tests {
    use crate::bonsai::{Event, Packet, Tree};
    use crate::wiring::{Core, EdgeIn};

    #[test]
    fn a_lower_limit_sets_off_an_alarm() {
        let mut core = Core::new();
        let limit = Packet::new("limit 260");
        core.handle(Event::Edge(EdgeIn::Uplink(limit)));
        core.handle(Event::Tick(0)); // sensor, the first branch: 26.5 °C
        assert_eq!(
            core.drain_uplink(),
            [Packet::new("ok, limit 260\n"), Packet::new("alarm 265\n")]
        );
    }
}
```

`Event::Tick(0)` is a tick for branch 0, the first in `bonsai.toml`.

```
test tests::a_lower_limit_sets_off_an_alarm ... ok

test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

```sh
git add -A && git commit -m "An uplink"
```

## More edges

- [UDP](../guides/edges-udp.md): multicast, replying to whoever sent.
- [TCP](../guides/edges-tcp.md): a client that reconnects, or a server for
  several clients.
- [Serial](../guides/edges-serial.md): a UART or USB-serial adapter.
- [MAVLink](../guides/edges-mavlink.md): flight controllers and ground
  stations.
- [Custom](../guides/edges-custom.md): an edge of your own.

Next: [Watching a tree](08-watching.md).

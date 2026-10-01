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
link it one way or both: `bonsai link uplink <branch>` (what it receives), `bonsai link <branch> uplink` (what it sends)
updated src/links.rs
warning: edge uplink isn't linked: `bonsai link uplink <branch>` to hear it, `bonsai link <branch> uplink` to send
```

It listens on port 6969, and sends to port 6970 on this computer (in real
life, the address of whatever should hear the alarms). In `bonsai.toml`:

```toml
[edge.uplink]
kind = "udp"
bind = "0.0.0.0:6969"
to = "127.0.0.1:6970"
```

## Link it

A link with an edge at one end carries its packets, so it takes no message
name:

```sh
bonsai link uplink watchdog
bonsai link watchdog uplink
bonsai list
```

```
uplink --> watchdog: what uplink receives arrives as `Input::Uplink(..)`
updated src/links.rs
watchdog --> uplink: send from watchdog with `out.to_uplink(..)`
updated src/links.rs
tree: greenhouse  (zero-2w (bcm2710a1))
branches, in the order the core runs them:
  sensor  (ticks 1/s)
  watchdog  (settings: limit)
  display
edges:
  uplink  (udp 0.0.0.0:6969 → 127.0.0.1:6970)
links:
  sensor --Reading--> watchdog, display
  watchdog --Alarm--> display
  uplink --> watchdog
  watchdog --> uplink
record: events, panics → logs/
```

The watchdog gained an `Input::Uplink(_uplink) => {}` arm, a test for it
(`on_uplink`, which hands it `Packet::new("hello")`), and an
`out.to_uplink(packet)` to send with.

## Decide what the bytes mean

The edge moves bytes; the watchdog decides what they mean. That keeps the
decoding in `process`, where a test can reach it. Plain text is enough to
start: the watchdog sends `alarm 31.0` when it's too hot, and takes `limit
28` to set a new limit (in °C), answering the sender. (For compact binary
messages, see the [binary messages guide](../guides/binary-messages.md).)

In `src/branches/watchdog.rs`, bring `Packet` in:

```rust
use crate::bonsai::{Branch, Packet};
```

and fill in `process`:

```rust
fn process(&mut self, input: Input, out: &mut Out) {
    match input {
        // `bonsai link <from> <Message> watchdog` adds an arm here
        Input::Reading(reading) => {
            if reading.temp > self.limit {
                out.send(Alarm { temp: reading.temp });
                let text = format!("alarm {:.1}\n", reading.temp.0);
                out.to_uplink(Packet::new(text));
            }
        }
        Input::Uplink(packet) => {
            // `limit 28` sets a new limit, in °C.
            let bytes = String::from_utf8_lossy(&packet.bytes);
            let text = bytes.trim();
            if let Some(number) = text.strip_prefix("limit ")
                && let Ok(limit) = number.parse::<f32>()
            {
                self.limit = Celsius(limit);
                info!("limit is now {:.1}", self.limit);
                out.to_uplink(packet.reply(format!("ok, limit {:.1}\n", self.limit)));
            } else {
                warn!("didn't understand {text:?}");
            }
        }
        // bonsai:input-arm
    }
}
```

The number goes out bare (`reading.temp.0`): the other end of a text
protocol reads numbers, not symbols. `Packet::new(text)` goes to the edge's
`to` address. `packet.reply(..)`
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
echo "limit 28" | socat - UDP:127.0.0.1:6969
echo "hello" | socat - UDP:127.0.0.1:6969
```

```
ok, limit 28.0 °C
```

The listener shows the alarms: three at the old limit, then one at 29.5 °C
under the new one:

```
alarm 31.0
alarm 32.5
alarm 34.0
alarm 29.5
```

And the tree's log:

```
11:55:38.570Z  INFO bonsai: running
11:55:38.570Z  INFO uplink: up
11:55:38.571Z  INFO display: 26.5 °C, 55% humidity
…
11:55:43.571Z  INFO display: 34.0 °C, 55% humidity
11:55:43.571Z  WARN display: too hot: 34.0 °C
11:55:44.028Z  INFO watchdog: limit is now 28.0 °C
11:55:44.533Z  WARN watchdog: didn't understand "hello"
11:55:44.572Z  INFO display: 25.0 °C, 55% humidity
11:55:45.572Z  INFO display: 26.5 °C, 55% humidity
11:55:46.571Z  INFO display: 28.0 °C, 55% humidity
11:55:47.572Z  INFO display: 29.5 °C, 55% humidity
11:55:47.572Z  WARN display: too hot: 29.5 °C
11:55:48.051Z  INFO bonsai: stopping (Ctrl-C)
```

## When an edge fails

Start the tree while something else holds port 6969, and the edge can't
open. It tries again, waiting longer each time, while the rest of the tree
carries on. Once the port is free, it comes up:

```
11:55:57.792Z  INFO bonsai: running
11:55:57.792Z  WARN uplink: bind 0.0.0.0:6969: Address already in use (os error 98); retrying in 100ms
11:55:57.793Z  INFO display: 26.5 °C, 55% humidity
11:55:57.894Z  WARN uplink: bind 0.0.0.0:6969: Address already in use (os error 98); retrying in 200ms
11:55:58.096Z  WARN uplink: bind 0.0.0.0:6969: Address already in use (os error 98); retrying in 400ms
11:55:58.498Z  WARN uplink: bind 0.0.0.0:6969: Address already in use (os error 98); retrying in 800ms
11:55:58.794Z  INFO display: 28.0 °C, 55% humidity
11:55:59.299Z  WARN uplink: bind 0.0.0.0:6969: Address already in use (os error 98); retrying in 1.6s
11:55:59.793Z  INFO display: 29.5 °C, 55% humidity
11:56:00.794Z  INFO display: 31.0 °C, 55% humidity
11:56:00.794Z  WARN display: too hot: 31.0 °C
11:56:00.905Z  INFO uplink: up
```

The same happens when a TCP connection drops or a serial port is unplugged.

## Tests

Run the tests again and one fails:

```sh
cargo local-test
```

```
---- branches::watchdog::tests::on_reading stdout ----

thread 'branches::watchdog::tests::on_reading' (12272) panicked at src/branches/watchdog.rs:80:9:
assertion failed: matches!(out.sent(), [Msg::WatchdogAlarm(alarm)] if alarm.temp ==
    Celsius(31.0))
```

Good: the watchdog now sends two things above the limit, and the test says
so. Update what it expects:

```rust
        assert!(matches!(
            out.sent(),
            [Msg::WatchdogAlarm(alarm), Msg::WatchdogToUplink(_)]
                if alarm.temp == Celsius(31.0)
        ));
```

A test can also drive the whole tree, edges included, with no sockets.
`core.from_uplink(packet)` hands the core a packet as if the uplink had
received it, `core.tick_sensor()` gives the sensor one tick of its rate, and
each runs everything it sets off, in order, as the running tree would.
`drain_uplink()` returns everything sent to the edge. Add this to the bottom
of `src/main.rs`:

```rust
#[cfg(test)]
mod tests {
    use crate::bonsai::Packet;
    use crate::links::Core;

    #[test]
    fn a_lower_limit_sets_off_an_alarm() {
        let mut core = Core::new();
        core.from_uplink(Packet::new("limit 26"));
        core.tick_sensor(); // 26.5 °C
        assert_eq!(
            core.drain_uplink(),
            [Packet::new("ok, limit 26.0 °C\n"), Packet::new("alarm 26.5\n")]
        );
    }
}
```

There's a `tick_<branch>()` for every branch with a rate, and a
`from_<edge>(..)` for every edge, so the compiler tells you if a test still
ticks a branch whose rate you took off.

```
test tests::a_lower_limit_sets_off_an_alarm ... ok

test result: ok. 19 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

```sh
git add -A && git commit -m "An uplink"
```

## More edges

- [UDP](../guides/edges-udp.md): multicast, replying to whoever sent.
- [TCP](../guides/edges-tcp.md): a client that reconnects, or a server for
  several clients.
- [Serial](../guides/edges-serial.md): a UART or USB-serial adapter.
- [Custom](../guides/edges-custom.md): an edge of your own.

Next: [Watching a tree](08-watching.md).

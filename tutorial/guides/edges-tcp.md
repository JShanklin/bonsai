# Edges: TCP

A TCP client that reconnects by itself, and a TCP server that takes any
number of clients.

**Needs:** [chapter 7](../foundations/07-edges.md).

TCP fits when every message must arrive, in order, over a link that can
drop: a base station, a server, a flight controller's TCP port, a web of
companion tools.

## Settings

```sh
bonsai edge add <name> tcp --connect HOST:PORT [--framing lines]   # a client
bonsai edge add <name> tcp --listen ADDR:PORT [--framing lines]    # a server
```

| key | means |
|-----|-------|
| `connect` | a client: connect there; when the connection drops or fails, try again (0.1 s, 0.2 s, … up to 5 s apart) |
| `listen` | a server: accept clients there, as many as connect |
| `framing` | `raw` (the default): each read is one packet, for protocols that frame themselves, like MAVLink; `lines`: one packet per line, without its newline, and a newline added to each packet sent |

A **client**'s packets carry the server's address as `peer`. A **server**'s
carry the client that sent them; a packet out goes to its `peer`, or to
every client when `peer` is `None`.

## A relay

A hub that clients connect to, where every line from a client goes to every
client and on to a station, and every line from the station goes to all the
clients:

```sh
bonsai edge add hub tcp --listen 0.0.0.0:7000 --framing lines
bonsai edge add station tcp --connect 127.0.0.1:7100 --framing lines
bonsai branch add relay
bonsai wire hub relay
bonsai wire station relay
bonsai wire relay hub station
```

```
added tcp edge hub
…
relay --> hub, station: send from relay with `out.to_hub(..)`, `out.to_station(..)`
updated src/wiring.rs
```

`bonsai.toml`:

```toml
[edge.hub]
kind = "tcp"
listen = "0.0.0.0:7000"
framing = "lines"

[edge.station]
kind = "tcp"
connect = "127.0.0.1:7100"
framing = "lines"
```

`src/branches/relay.rs` (bring `Packet` in with `use crate::bonsai::{Branch,
Packet};`):

```rust
fn process(&mut self, input: Input, out: &mut Out) {
    match input {
        // `bonsai wire <from> <Message> relay` adds an arm here
        Input::Hub(line) => {
            let text = String::from_utf8_lossy(&line.bytes);
            if let Some(from) = line.peer {
                // No peer: to every client.
                out.to_hub(Packet::new(format!("{from}: {text}")));
            }
            out.to_station(Packet::new(line.bytes));
        }
        Input::Station(line) => {
            let text = String::from_utf8_lossy(&line.bytes);
            out.to_hub(Packet::new(format!("station: {text}")));
        }
        // bonsai:input-arm
    }
}
```

## Try it

Run the tree. Nothing is listening on 7100 yet, so the station keeps
trying:

```
10:53:46.418Z  INFO bonsai: running
10:53:46.418Z  INFO hub: up
10:53:46.419Z  WARN station: connect 127.0.0.1:7100: Connection refused (os error 111); retrying in 100ms
10:53:46.521Z  WARN station: connect 127.0.0.1:7100: Connection refused (os error 111); retrying in 200ms
10:53:46.723Z  WARN station: connect 127.0.0.1:7100: Connection refused (os error 111); retrying in 400ms
```

Connect two clients, each in its own terminal, and type `hello` in the
first:

```sh
socat - TCP:127.0.0.1:7000
```

Both see it:

```
127.0.0.1:45820: hello
```

Now start a station in a third terminal:

```sh
socat - TCP-LISTEN:7100,reuseaddr
```

The station edge connects at its next try, and gets the `hello` it was sent
while it was down, then everything after:

```
10:53:47.927Z  INFO station: up
```

```
hello
to the station
```

Type `weather ok` into the station, and both clients get
`station: weather ok`. Stop the station (Ctrl-C), and the edge goes back to
trying:

```
10:53:52.429Z  WARN station: closed; retrying in 1.6s
10:53:54.030Z  WARN station: connect 127.0.0.1:7100: Connection refused (os error 111); retrying in 3.2s
```

The hub, the relay and the clients carry on the whole time.

## Notes

- While an edge is down, what branches send it waits in a queue of 64. If
  that fills, the rest is dropped and counted (`bonsai top` shows it), so
  a long outage doesn't grow memory without end.
- A client that disconnects from a server edge is simply forgotten; sends
  to it are skipped.
- For binary protocols, keep `framing = "raw"` and put the parsing in
  `process`; see the [wire format](wire-format.md) and
  [MAVLink](edges-mavlink.md) guides.

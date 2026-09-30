# Edges: TCP

A TCP client that reconnects by itself, and a TCP server that takes any
number of clients.

**Needs:** [chapter 7](../foundations/07-edges.md).

TCP fits when every message must arrive, in order, over a link that can
drop: a base station, a server, a device's TCP port, a web of
companion tools.

## Settings

```sh
bonsai edge add <name> tcp --connect HOST:PORT [--framing lines]   # a client
bonsai edge add <name> tcp --listen ADDR:PORT [--framing lines]    # a server
```

| key | means |
|-----|-------|
| `connect` | a client: connect there; when the connection drops or fails, try again (0.1 s, 0.2 s, … up to 5 s apart) |
| `listen` | a server: accept clients there, up to `max_clients` at once |
| `framing` | `raw` (the default): each read is one packet, for protocols that frame themselves; `lines`: one packet per line, without its newline, and a newline added to each packet sent |
| `max_frame` | the longest line, in bytes, with `lines` framing (default 1 MiB, `MAX_FRAME`): its payload, without the `\n` or `\r\n`, so `abcd\n` and `abcd\r\n` are both 4. Every line is held to it, however the bytes arrive (several lines in one read, or one line over many); a longer one is refused as soon as it must be longer, before it's buffered: a client edge reconnects, a server closes that client |
| `max_clients` | a server's most clients at once (default 64, `MAX_CLIENTS`). Past it, a client that has stopped sending makes room; when every client is still sending, the new connection is closed at once, with one warning |

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
bonsai link hub relay
bonsai link station relay
bonsai link relay hub station
```

```
added tcp edge hub
…
relay --> hub, station: send from relay with `out.to_hub(..)`, `out.to_station(..)`
updated src/links.rs
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
        // `bonsai link <from> <Message> relay` adds an arm here
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

- What branches send an edge waits in one queue of 64, kept across
  restarts: while the edge is down it waits there. If that fills, the rest
  is dropped and counted (`bonsai top`'s **dropped**), so a long outage
  doesn't grow memory without end, and the core never waits. A message the
  edge was carrying out when it failed is counted as **lost**.
- A server client that stops sending (it closed its side, or sent a line
  past `max_frame`) keeps its connection for 2 s (`LINGER`), so replies to
  what it sent still reach it; broadcasts skip it. Then it's closed, and
  its task ends. A client whose write fails is closed at once.
- When a server edge restarts, every client's connection is closed and its
  tasks end with it; clients reconnect to the new one.
- **Slow clients can't hold anyone up.** A server writes to each client
  from its own task, through its own queue of 64 packets. A client that
  stops reading misses what's sent while its queue is full, and once one
  write to it has waited 5 s (`WRITE_TIMEOUT`) it's disconnected, without
  re-sending what was half-written. Everyone else, and what the edge
  receives, carries on meanwhile. What a client missed is counted as
  **lost** in `bonsai top`.
- For binary protocols, keep `framing = "raw"` and put the parsing in
  `process`; see the [binary messages](binary-messages.md) guide.

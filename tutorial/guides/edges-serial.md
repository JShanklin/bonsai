# Edges: serial

Talk to a device on a UART or a USB-serial adapter: a GPS, a flight
controller, a microcontroller.

**Needs:** [chapter 7](../foundations/07-edges.md).

## Settings

```sh
bonsai edge add <name> serial --device /dev/serial0 --baud 9600 [--framing lines]
```

| key | means |
|-----|-------|
| `device` | the port: `/dev/serial0` (a Pi's UART), `/dev/ttyUSB0` or `/dev/ttyACM0` (USB adapters and boards) |
| `baud` | its speed, as the device expects it: 9600 for most GPS receivers, 57600 or 115200 for flight controllers |
| `framing` | `raw` (the default): whatever each read returns, for protocols with their own framing (MAVLink); `lines`: one packet per line, without its `\r\n`, and a newline added to each packet sent |

A serial packet's `peer` is always `None`.

The first serial edge adds the `tokio-serial` crate to `Cargo.toml` and
writes `src/edges/serial.rs`; removing the last one takes both out again.
It needs no system libraries, so it cross-builds for every Pi, the Zero W
included.

## A GPS

A branch that logs each position fix, and every two seconds asks the
receiver whether it's there:

```sh
bonsai edge add gps serial --device /dev/serial0 --baud 9600 --framing lines
bonsai branch add position
bonsai wire gps position
bonsai wire position gps
bonsai rate position 0.5
```

```
added serial edge gps
wire it with `bonsai wire gps <branch>` (what it receives) and `bonsai wire <branch> gps` (what it sends)
updated src/wiring.rs, src/edges/mod.rs, src/edges/serial.rs, Cargo.toml (+tokio-serial)
…
position ticks 0.5 times a second: `Input::Tick` in its process
```

`src/branches/position.rs` (with `use crate::bonsai::{Branch, Packet};`):

```rust
fn process(&mut self, input: Input, out: &mut Out) {
    match input {
        // `bonsai wire <from> <Message> position` adds an arm here
        Input::Gps(line) => {
            let text = String::from_utf8_lossy(&line.bytes);
            if text.starts_with("$GPGGA") {
                info!("fix: {text}");
            } else {
                debug!("{text}");
            }
        }
        Input::Tick => {
            // Ask the receiver if it's there: an MTK "test" sentence.
            out.to_gps(Packet::new("$PMTK000*32"));
        }
        // bonsai:input-arm
    }
}
```

## Try it without a device

`socat` can make a pair of connected virtual serial ports: what's written to
one comes out of the other. In one terminal:

```sh
socat pty,raw,echo=0,link=/tmp/ttyV0 pty,raw,echo=0,link=/tmp/ttyV1
```

Point the edge at one end (`device = "/tmp/ttyV0"` in `bonsai.toml`, then
`bonsai sync`), and run the tree with the position branch's debug lines on:

```sh
BONSAI_LOG=info,position=debug cargo local
```

Play the GPS at the other end. Watch what the tree sends it:

```sh
cat /tmp/ttyV1
```

```
$PMTK000*32
$PMTK000*32
```

and send it two sentences:

```sh
printf '$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*47\r\n$GPRMC,123519,A,4807.038,N,01131.000,E,022.4,084.4,230394,003.1,W*6A\r\n' > /tmp/ttyV1
```

```
10:55:22.981Z  INFO bonsai: running
10:55:22.981Z  INFO gps: up
10:55:23.981Z  INFO position: fix: $GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*47
10:55:23.981Z DEBUG position: $GPRMC,123519,A,4807.038,N,01131.000,E,022.4,084.4,230394,003.1,W*6A
```

Unplug a real adapter, or stop the socat, and the edge logs the error and
reopens the port when it's back.

## On a Raspberry Pi

- **Turn on the UART:** `sudo raspi-config` → Interface Options → Serial
  Port: *no* to a login shell over serial, *yes* to the serial port
  hardware. Reboot. `/dev/serial0` then points at the right UART.
- **Permission:** the user running the tree must be in the `dialout` group
  (`sudo usermod -aG dialout $USER`, then log in again). Otherwise the edge
  logs `Permission denied` and keeps retrying.
- **Check the device first:** `ls -l /dev/serial0` shows which UART it is;
  `sudo cat /dev/serial0` shows whether anything is arriving at all.
- **Binary protocols** (MAVLink, UBX): keep `framing = "raw"` and parse in
  `process`; see the [MAVLink guide](edges-mavlink.md).

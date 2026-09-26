# Roots: serial

Talk to another device over a UART or a USB-serial adapter: a flight
controller, a GPS, another microcontroller.

**Needs:** [chapter 7](../foundations/07-roots.md) and the [wire format](wire-format.md).
**Crate:** [serialport](https://docs.rs/serialport). Without default
features it has no system-library dependency, so it cross-compiles cleanly.

```sh
cargo add serialport --no-default-features
```

## Wire it

```sh
bonsai branch --roots port
bonsai tap port Reading
bonsai tap port Alarm
bonsai release port Setpoint
```

## Fill in `src/branches/port.rs`

Imports:

```rust
use std::io::{BufRead, BufReader, ErrorKind};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serialport::SerialPort;

use crate::wire::{self, Wire};
```

Types and settings:

```rust
/// A decoded message from the other device.
pub type Inbound = Wire;
/// One encoded frame for the other device.
pub type Outbound = Vec<u8>;

const DEVICE: &str = "/dev/serial0";
const BAUD: u32 = 115_200;
```

In `start`, the port can be unplugged and reopened, so both threads share it
the same way the [TCP guide](roots-tcp.md) shares its connection:

```rust
    // The open port, shared by both threads. `None` while unplugged.
    let port: Arc<Mutex<Option<Box<dyn SerialPort>>>> = Arc::default();
    let writer = port.clone();
    let (inbox, outbox) = roots::bridge(
        "port",
        move |deliver| loop {
            let opened = match serialport::new(DEVICE, BAUD)
                .timeout(Duration::from_secs(1))
                .open()
            {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("port: {DEVICE}: {e}");
                    thread::sleep(Duration::from_secs(1));
                    continue;
                }
            };
            *port.lock().unwrap() = opened.try_clone().ok();
            let mut reader = BufReader::new(opened);
            let mut frame = Vec::new();
            loop {
                match reader.read_until(0, &mut frame) {
                    Ok(_) if frame.last() == Some(&0) => {
                        if let Some(msg) = wire::decode(&mut frame) {
                            deliver(msg);
                        }
                        frame.clear();
                    }
                    Ok(_) => {}
                    // A quiet second: keep the partial frame and read on.
                    Err(e) if e.kind() == ErrorKind::TimedOut => {}
                    // Unplugged: reopen.
                    Err(_) => break,
                }
            }
            *port.lock().unwrap() = None;
        },
        move |frame: Outbound| {
            if let Some(port) = writer.lock().unwrap().as_mut() {
                let _ = port.write_all(&frame);
            }
        },
    );
```

The one-second timeout is there so a read never waits forever on a dead
line. `run` is the same as in the [UDP guide](roots-udp.md#fill-in-srcbranchesuplinkrs).

## How it behaves

- **Unplugged adapter:** reading fails, and the receive thread retries
  opening every second until it's back.
- **Quiet line:** the 1-second timeout just loops. A frame split across a
  pause is kept and completed.
- **Corrupt bytes:** that frame fails to decode and is skipped. The next
  frame starts cleanly after the next `0`.

## On a Raspberry Pi

- Run `sudo raspi-config`. Under *Interface Options → Serial Port*, say **no**
  to a login shell and **yes** to the hardware.
- Add yourself to the `dialout` group:
  `sudo usermod -aG dialout $USER`, then log in again.
- `/dev/serial0` is the header pins (GPIO 14/15). A USB adapter shows up as
  `/dev/ttyUSB0` or `/dev/ttyACM0`.
- On a Zero W / Zero 2 W, add `dtoverlay=disable-bt` to `/boot/firmware/config.txt`.
  That gives the header pins the full UART, which keeps high baud rates
  reliable. A Pi 5 doesn't need this: Bluetooth doesn't share its header UART.

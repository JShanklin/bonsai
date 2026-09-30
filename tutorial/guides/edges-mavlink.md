# Edges: MAVLink

Talk to a flight controller (PX4, ArduPilot) or a ground station over
MAVLink: send heartbeats as the vehicle's onboard computer, and read what
the vehicle says.

**Needs:** [chapter 7](../foundations/07-edges.md) and the
[UDP guide](edges-udp.md).
**Crate:** [mavlink](https://docs.rs/mavlink). It does the framing, the
checksums and the message types, so there's no wire format to write.

## How it fits

A UDP edge carries raw MAVLink datagrams; a branch decodes and encodes them
with the crate. The decoding stays in `process`, so a test can hand it
frames. That's the same split as every edge: the edge moves bytes, the
branch decides what they mean.

```
flight controller ──UDP 14550──▶ fc edge ──▶ link branch ──▶ (your branches)
                  ◀── heartbeats ──────────────┘
```

On a Pi wired to the flight controller's serial port, run
[mavlink-router](https://github.com/mavlink-router/mavlink-router) on the Pi:
it owns the serial port and hands MAVLink to the tree (and to a ground
station) over UDP. With PX4's simulator, the onboard link is UDP already
(`14540`).

## Set it up

```sh
cargo add mavlink --no-default-features --features std,dialect-common
bonsai edge add fc udp --bind 0.0.0.0:14550 --reply
bonsai branch add link
bonsai wire fc link
bonsai wire link fc
bonsai rate link 1
```

Only `std` and the `common` dialect are compiled in: the crate's defaults
bring its own transports, serde and ArduPilot's dialect, none of which the
tree needs (use `dialect-ardupilotmega` for ArduPilot's own messages).
`--reply` sends back to whoever sent last, the flight controller or the
router, so the tree needs no fixed address.

## The link branch

`src/branches/link.rs`:

```rust
use mavlink::dialects::common::{self as mav, MavMessage};
use mavlink::peek_reader::PeekReader;
use mavlink::{MavHeader, read_v2_msg, write_v2_msg};

use crate::bonsai::{Branch, Packet};
#[allow(unused_imports)] // the messages it builds to send
use crate::messages::*;
use crate::wiring::link::{Input, Out};

/// What link keeps between inputs.
pub struct Link {
    /// Counts every frame sent, as MAVLink asks.
    sequence: u8,
    /// Whether the vehicle was armed, when last heard.
    armed: Option<bool>,
}

impl Link {
    /// A frame from this computer: system 1 (the vehicle's), the onboard
    /// computer's component id.
    fn frame(&mut self, message: &MavMessage) -> Packet {
        let header = MavHeader {
            system_id: 1,
            component_id: mav::MavComponent::MAV_COMP_ID_ONBOARD_COMPUTER as u8,
            sequence: self.sequence,
        };
        self.sequence = self.sequence.wrapping_add(1);
        let mut bytes = Vec::new();
        // Writing into a Vec can't fail.
        let _ = write_v2_msg(&mut bytes, header, message);
        Packet::new(bytes)
    }
}

impl Branch for Link {
    type Input = Input;
    type Out = Out;

    fn setup() -> Self {
        Link {
            sequence: 0,
            armed: None,
        }
    }

    fn process(&mut self, input: Input, out: &mut Out) {
        match input {
            // `bonsai wire <from> <Message> link` adds an arm here
            Input::Tick => {
                let heartbeat = MavMessage::HEARTBEAT(mav::HEARTBEAT_DATA {
                    mavtype: mav::MavType::MAV_TYPE_ONBOARD_CONTROLLER,
                    autopilot: mav::MavAutopilot::MAV_AUTOPILOT_INVALID,
                    system_status: mav::MavState::MAV_STATE_ACTIVE,
                    mavlink_version: 3,
                    ..Default::default()
                });
                let packet = self.frame(&heartbeat);
                out.to_fc(packet);
            }
            Input::Fc(packet) => {
                // One datagram can hold several frames: read them all.
                let mut frames = PeekReader::new(&packet.bytes[..]);
                while let Ok((header, message)) = read_v2_msg::<MavMessage, _>(&mut frames) {
                    match message {
                        MavMessage::HEARTBEAT(heartbeat) => {
                            let armed = heartbeat
                                .base_mode
                                .contains(mav::MavModeFlag::MAV_MODE_FLAG_SAFETY_ARMED);
                            if self.armed != Some(armed) {
                                let state = if armed { "armed" } else { "disarmed" };
                                info!("system {} is {state}", header.system_id);
                                self.armed = Some(armed);
                            }
                        }
                        MavMessage::ATTITUDE(attitude) => {
                            let roll = attitude.roll.to_degrees();
                            let pitch = attitude.pitch.to_degrees();
                            debug!("roll {roll:.1}°, pitch {pitch:.1}°");
                        }
                        _ => {}
                    }
                }
            }
            // bonsai:input-arm
        }
    }
}
```

`while let` repeats while the pattern fits: here, until there's no whole
frame left in the datagram. Only logging what changed (armed or not) keeps a
50 Hz stream from flooding the log; the attitude goes to `debug!`, hidden
unless asked for.

To pass what it hears on, add messages (`bonsai message add Attitude
roll:f32 pitch:f32`) and wire them from `link` to the branches that decide,
like any other.

## Try it

Against a real flight controller or PX4's simulator, point it (or
mavlink-router) at the tree's port. Without one, pymavlink can play the
flight controller: `pip install pymavlink`, then save this as `fc.py`:

```python
# A pretend flight controller: heartbeats and attitude to the tree, and
# prints the heartbeats it hears back.
import time, math
from pymavlink import mavutil
m = mavutil.mavlink_connection('udpout:127.0.0.1:14550', source_system=1, source_component=1)
m.mav.WIRE_PROTOCOL_VERSION = "2.0"
start = time.time()
armed = 0
while time.time() - start < 5:
    t = time.time() - start
    if t > 2.5: armed = mavutil.mavlink.MAV_MODE_FLAG_SAFETY_ARMED
    m.mav.heartbeat_send(mavutil.mavlink.MAV_TYPE_QUADROTOR, mavutil.mavlink.MAV_AUTOPILOT_PX4, armed, 0, mavutil.mavlink.MAV_STATE_ACTIVE)
    m.mav.attitude_send(int(t*1000), math.radians(5*math.sin(t)), math.radians(-2.0), 0.0, 0, 0, 0)
    msg = m.recv_match(type='HEARTBEAT', blocking=True, timeout=1)
    if msg:
        print(f"heartbeat from {msg.get_srcSystem()}/{msg.get_srcComponent()}: type {msg.type}, autopilot {msg.autopilot}")
```

Run the tree with the link's debug lines on, and the script alongside:

```sh
BONSAI_LOG=info,link=debug cargo local
MAVLINK20=1 python3 fc.py
```

The script hears the tree's heartbeats, from component 191 (the onboard
computer), type 18 (`MAV_TYPE_ONBOARD_CONTROLLER`):

```
heartbeat from 1/191: type 18, autopilot 8
heartbeat from 1/191: type 18, autopilot 8
```

and the tree follows the vehicle:

```
10:57:52.437Z  INFO bonsai: running
10:57:52.437Z  INFO fc: up
10:57:53.082Z  INFO link: system 1 is disarmed
10:57:53.082Z DEBUG link: roll 0.0°, pitch -2.0°
10:57:53.440Z DEBUG link: roll 1.8°, pitch -2.0°
10:57:54.439Z DEBUG link: roll 4.9°, pitch -2.0°
10:57:56.439Z  INFO link: system 1 is armed
10:57:56.439Z DEBUG link: roll -1.1°, pitch -2.0°
```

## Test it

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::bonsai::{Event, Tree};
    use crate::wiring::Core;

    #[test]
    fn each_tick_sends_a_heartbeat() {
        let mut core = Core::new();
        core.handle(Event::Tick(0));
        let sent = core.drain_fc();
        let mut frames = PeekReader::new(&sent[0].bytes[..]);
        let (header, message) = read_v2_msg::<MavMessage, _>(&mut frames).unwrap();
        assert_eq!(header.component_id, 191);
        assert!(matches!(message, MavMessage::HEARTBEAT(_)));
    }
}
```

In a test, `.unwrap()` is fine: a panic is exactly how a test fails.

## Notes

- **Size:** this tree builds to about 630 KB for a Pi (release,
  `aarch64`).
- **MAVLink 1:** `read_v2_msg` skips version 1 frames. Current PX4 and
  ArduPilot speak version 2; if yours sends version 1, set its link to
  MAVLink 2 (ArduPilot: the port's `SERIALn_PROTOCOL = 2`).
- **Serial:** a raw serial edge's reads can split a frame in two, so a
  branch reading straight from one must keep the unfinished bytes between
  inputs. mavlink-router, in front of the tree, does this for you.

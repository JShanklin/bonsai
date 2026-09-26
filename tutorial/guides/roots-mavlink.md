# Roots: MAVLink

Talk to a ground station (QGroundControl, Mission Planner) or a flight
controller over MAVLink: send heartbeats and telemetry, take commands.

**Needs:** [chapter 7](../foundations/07-roots.md).
**Crate:** [mavlink](https://docs.rs/mavlink). It does the framing, the
checksums and the message types, so there's no wire format to write.

```sh
cargo add mavlink --no-default-features --features std,transport-udp,dialect-ardupilotmega
```

Only the transport and dialect you use are compiled in. For a serial link,
add `transport-direct-serial`. For TCP, add `transport-tcp`.

## Wire it

The ground station sends an arm command in, and the tree sends its attitude
out:

```sh
bonsai branch --roots gcs
bonsai feed Arm --directed on:bool
bonsai feed Attitude roll:f32 pitch:f32 yaw:f32
bonsai release gcs Arm
bonsai tap gcs Attitude
```

`Arm` is directed because a command must never be dropped. Your own
branches tap `Arm` and release `Attitude`.

## Fill in `src/branches/gcs.rs`

Imports:

```rust
use std::sync::Arc;

use embassy_futures::select::{Either3, select3};
use embassy_time::{Duration, Ticker};
use mavlink::dialects::ardupilotmega as apm;
use mavlink::{MavConnection, MavHeader};
```

and change the scaffold's `select` import to `select3` (above).

Both directions carry MAVLink messages, so the crate's own message type is
the root's message type:

```rust
/// A MAVLink message from the ground station.
pub type Inbound = apm::MavMessage;
/// A MAVLink message for the ground station.
pub type Outbound = apm::MavMessage;

const ADDRESS: &str = "udpout:127.0.0.1:14550";
/// Who we are on the MAVLink network: vehicle 1, its autopilot (component 1).
const HEADER: MavHeader = MavHeader {
    system_id: 1,
    component_id: 1,
    sequence: 0,
};
```

`sequence` is filled in by the crate as it sends. Give each vehicle on a
network its own `system_id`.

### `start`

One connection, shared by both threads with an `Arc`:

```rust
    let conn = mavlink::connect::<apm::MavMessage>(ADDRESS).expect("gcs: can't open the link");
    let conn = Arc::new(conn);
    let sender = conn.clone();
    let (inbox, outbox) = roots::bridge(
        "gcs",
        move |deliver| loop {
            if let Ok((_header, msg)) = conn.recv() {
                deliver(msg);
            }
        },
        move |msg: Outbound| {
            let _ = sender.send(&HEADER, &msg);
        },
    );
```

A frame that fails its checksum, or a message the dialect doesn't know, is an
`Err` and is skipped.

### `run`

A ground station drops a vehicle it hasn't heard from in a few seconds, so
the root also sends a heartbeat every second. A `Ticker` is a third thing for
`run` to wait on, so `select` becomes `select3` and `Either` becomes `Either3`:

```rust
#[embassy_executor::task]
async fn run(mut taps: sap::gcs::Taps, sap: Sap, inbox: Inbox<Inbound>, outbox: Outbox<Outbound>) {
    // A ground station drops a vehicle it hasn't heard from in a few seconds.
    let mut heartbeat = Ticker::every(Duration::from_secs(1));
    loop {
        // Wait for whichever comes first: a message, a nutrient, or the next heartbeat.
        match select3(inbox.next(), taps.next(), heartbeat.next()).await {
            Either3::First(msg) => {
                if let apm::MavMessage::COMMAND_LONG(cmd) = msg
                    && cmd.command == apm::MavCmd::MAV_CMD_COMPONENT_ARM_DISARM
                {
                    let on = cmd.param1 == 1.0; // 1 = arm, 0 = disarm
                    sap.release(Nutrient::Arm { on }).await;
                }
                // bonsai:emit
            }
            Either3::Second(nutrient) => {
                #[allow(clippy::single_match, clippy::match_single_binding)]
                // until it taps more nutrients
                match nutrient {
                    Nutrient::Attitude { roll, pitch, yaw } => {
                        outbox.send(apm::MavMessage::ATTITUDE(apm::ATTITUDE_DATA {
                            roll,
                            pitch,
                            yaw,
                            ..Default::default()
                        }));
                    }
                    // bonsai:nutrient-arm
                    #[allow(unreachable_patterns)]
                    _ => {}
                }
            }
            Either3::Third(()) => {
                outbox.send(apm::MavMessage::HEARTBEAT(apm::HEARTBEAT_DATA {
                    custom_mode: 0,
                    mavtype: apm::MavType::MAV_TYPE_QUADROTOR,
                    autopilot: apm::MavAutopilot::MAV_AUTOPILOT_ARDUPILOTMEGA,
                    base_mode: apm::MavModeFlag::empty(),
                    system_status: apm::MavState::MAV_STATE_STANDBY,
                    mavlink_version: 3,
                }));
            }
        }
    }
}
```

`..Default::default()` leaves the fields you don't send (the time stamp, the
rotation rates) at zero. See
[Structs](../foundations/02-rust-essentials.md#structs).

## Picking the address

| address | the tree… | use it when |
|---------|-----------|-------------|
| `udpout:192.168.1.10:14550` | sends to a fixed address | the ground station listens for vehicles (QGroundControl's default: it shows the vehicle once it hears a heartbeat) |
| `udpin:0.0.0.0:14550` | listens, and replies to whoever last sent to it | the peer connects to the vehicle (MAVProxy, a companion app). Nothing goes out until that peer sends something |
| `serial:/dev/serial0:57600` | uses a UART | a telemetry radio or a flight controller on the header pins (needs `transport-direct-serial`) |
| `tcpout:10.0.0.5:5760` | connects to a TCP server | SITL, or a MAVLink router (needs `transport-tcp`) |

Changing the address is the only change: `start` and `run` stay the same.

## Try it

Start QGroundControl on the same machine, then `cargo local`. QGC shows the
vehicle once it hears a heartbeat. Its arm command is a `COMMAND_LONG` with
`MAV_CMD_COMPONENT_ARM_DISARM`, which `run` turns into `Nutrient::Arm`.

Without QGC, a small program can play the ground station: listen on
`udpin:0.0.0.0:14550` with the same crate, print what arrives, and send a
`COMMAND_LONG` arm back. The tree's heartbeats arrive, and a branch that taps
`Arm` sees the command:

```
from sys=1 comp=1: HEARTBEAT
from sys=1 comp=1: HEARTBEAT
motors: armed = true
```

## Two links at once

A vehicle often talks to several peers: a ground station, a companion app, a
telemetry server. Give each its own root (`bonsai branch --roots telemetry`),
with its own address and message type. Several roots can tap the same
broadcast nutrient, such as `Attitude`, and each gets its own copy.

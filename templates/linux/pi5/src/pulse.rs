//! The pulse — bonsai's built-in cycle of life.
//!
//! The trunk always runs this: `heartbeat` releases a `Beat` nutrient into the
//! sap every half-second, and `monitor` taps it and prints each beat — a
//! visible sign that the executor and the sap are running.

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};

use crate::sap::{self, Sap};
use crate::trunk::{Nutrient, Trunk};

/// Start the pulse. The trunk calls this once at startup.
pub fn start(spawner: &Spawner, trunk: &Trunk) {
    spawner.spawn(
        heartbeat(trunk.sap()).unwrap_or_else(|_| panic!("pulse: heartbeat already running")),
    );
    spawner.spawn(
        monitor(sap::pulse::Taps::new())
            .unwrap_or_else(|_| panic!("pulse: monitor already running")),
    );
}

/// Releases a `Beat` into the sap every half-second.
#[embassy_executor::task]
async fn heartbeat(sap: Sap) {
    loop {
        Timer::after(Duration::from_millis(500)).await;
        sap.release(Nutrient::Beat).await;
    }
}

/// Taps the sap and logs each beat, so the tree's pulse is visible.
#[embassy_executor::task]
async fn monitor(mut taps: sap::pulse::Taps) {
    loop {
        let nutrient: Nutrient = taps.next().await;
        #[allow(clippy::single_match)] // until it taps more than one nutrient
        match nutrient {
            Nutrient::Beat => println!("bonsai: beat"),
            // bonsai:nutrient-arm
            #[allow(unreachable_patterns)]
            _ => {}
        }
    }
}

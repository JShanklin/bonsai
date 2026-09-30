//! {{project-name}} — Raspberry Pi Zero 2 W Linux application.
//! The trunk starts pulse and each grafted branch.

mod branches;
mod pulse;
mod trunk;

use embassy_executor::Spawner;
use trunk::Trunk;
use trunk::sap; // the generated sap lives under the trunk (src/sap.rs)

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let trunk = Trunk::new();
    pulse::start(&spawner, &trunk);

    // Start branches here. Pass Linux device handles to start() when needed.
    // bonsai:start
}

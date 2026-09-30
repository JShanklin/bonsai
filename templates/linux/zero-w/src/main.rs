//! {{project-name}} — Raspberry Pi Zero W Linux application.
//! The trunk: it starts the core, which runs every branch (see src/bonsai.rs).

mod bonsai;
mod branches;
mod edges;
mod messages;
#[rustfmt::skip]
mod settings;
#[rustfmt::skip]
mod wiring;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    bonsai::run(wiring::Core::new()).await;
}

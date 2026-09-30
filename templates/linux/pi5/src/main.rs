//! {{project-name}} — Raspberry Pi 5 Linux application.
//! The trunk: it starts the core, which runs every branch (see src/bonsai.rs).

#[macro_use]
mod bonsai;
mod branches;
mod edges;
#[rustfmt::skip]
mod links;
mod messages;
#[rustfmt::skip]
mod settings;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    bonsai::run(links::Core::new()).await;
}

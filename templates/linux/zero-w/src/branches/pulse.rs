//! The pulse: a heartbeat, visible proof the tree is running. Its `rate` in
//! bonsai.toml ticks it twice a second. Keep it, or `bonsai branch remove
//! pulse` once your own branches show the tree is alive.

use crate::bonsai::Branch;
use crate::wiring::pulse::{Input, Out};

pub struct Pulse {}

impl Branch for Pulse {
    type Input = Input;
    type Out = Out;

    fn setup() -> Self {
        Pulse {}
    }

    fn process(&mut self, input: Input, out: &mut Out) {
        let _ = out;
        match input {
            Input::Tick => println!("bonsai: beat"),
            // bonsai:input-arm
        }
    }
}

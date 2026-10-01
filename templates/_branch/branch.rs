//! The {{branch_name}} branch.
//! Grown by `bonsai branch add {{branch_name}}`.

use crate::bonsai::Branch;
use crate::links::{{branch_name}}::{Input, Out};
#[allow(unused_imports)] // the messages it sends, and units
use crate::messages::*;

/// What {{branch_name}} keeps between inputs.
pub struct {{BranchName}} {}

impl Branch for {{BranchName}} {
    type Input = Input;
    type Out = Out;

    /// Setup: the starting state. Runs again if `process` panics.
    fn setup() -> Self {
        {{BranchName}} {}
    }

    /// Process: decide what to do with each input, and `out.send(..)` the
    /// result. No I/O and no waiting, so the same inputs give the same outputs.
    /// Log with info!/warn!/debug!: lines are tagged with this branch.
    fn process(&mut self, input: Input, out: &mut Out) {
        let _ = out; // delete once it sends
        match input {
            // `bonsai link <from> <Message> {{branch_name}}` adds an arm here
            // bonsai:input-arm
        }
    }
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)] // what its tests use
    use super::*;
    #[allow(unused_imports)] // what it sends, as its tests see it
    use crate::links::Msg;

    // `bonsai link`/`rate` add a test per input here; `unlink` removes it unless edited
    // bonsai:input-test
}

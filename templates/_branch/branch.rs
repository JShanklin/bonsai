use crate::bonsai::Branch;
use crate::links::{{branch_name}}::{Input, Out};
#[allow(unused_imports)]
use crate::messages::*;

pub struct {{BranchName}} {}

impl Branch for {{BranchName}} {
    type Input = Input;
    type Out = Out;

    fn setup() -> Self {
        {{BranchName}} {}
    }

    fn process(&mut self, input: Input, out: &mut Out) {
        let _ = out; // delete once it sends
        match input {
            // bonsai:input-arm
        }
    }
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;
    #[allow(unused_imports)]
    use crate::links::Msg;

    // bonsai:input-test
}

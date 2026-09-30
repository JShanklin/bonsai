//! A branch that panics: set up again, and when even that fails, taken out
//! of service without taking the tree down or spinning.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use crate::bonsai::{Branch, Outbox, Slot, stats};

#[derive(Default)]
struct Out(Vec<u32>);

impl Outbox for Out {
    fn count(&self) -> usize {
        self.0.len()
    }
}

/// Behaviour switches for the test branches (each test has its own statics
/// through its own branch type).
macro_rules! branch {
    ($name:ident, $setups:ident, $setup_fails:ident, $process_fails:ident) => {
        static $setups: AtomicU64 = AtomicU64::new(0);
        static $setup_fails: AtomicBool = AtomicBool::new(false);
        static $process_fails: AtomicBool = AtomicBool::new(false);

        struct $name;

        impl Branch for $name {
            type Input = u32;
            type Out = Out;

            fn setup() -> Self {
                $setups.fetch_add(1, Relaxed);
                if $setup_fails.load(Relaxed) {
                    panic!("setup can't start");
                }
                $name
            }

            fn process(&mut self, input: u32, out: &mut Out) {
                out.0.push(input);
                if $process_fails.load(Relaxed) {
                    panic!("process failed on {input}");
                }
            }
        }
    };
}

branch!(
    Healthy,
    HEALTHY_SETUPS,
    HEALTHY_SETUP_FAILS,
    HEALTHY_PROCESS_FAILS
);
branch!(
    Broken,
    BROKEN_SETUPS,
    BROKEN_SETUP_FAILS,
    BROKEN_PROCESS_FAILS
);
branch!(
    Recovers,
    RECOVERS_SETUPS,
    RECOVERS_SETUP_FAILS,
    RECOVERS_PROCESS_FAILS
);
branch!(
    Stillborn,
    STILLBORN_SETUPS,
    STILLBORN_SETUP_FAILS,
    STILLBORN_PROCESS_FAILS
);

#[test]
fn a_panic_then_a_good_setup_keeps_working_as_before() {
    let mut slot = Slot::<Recovers>::new("rt_recovers");
    assert_eq!(slot.process(1).0, [1]);
    RECOVERS_PROCESS_FAILS.store(true, Relaxed);
    assert!(
        slot.process(2).0.is_empty(),
        "a panicked input's sends are dropped"
    );
    RECOVERS_PROCESS_FAILS.store(false, Relaxed);
    assert_eq!(slot.process(3).0, [3]);
    assert_eq!(RECOVERS_SETUPS.load(Relaxed), 2);
}

#[test]
fn a_branch_that_cannot_set_up_again_is_taken_out_not_the_tree() {
    let mut broken = Slot::<Broken>::new("rt_broken");
    let mut healthy = Slot::<Healthy>::new("rt_healthy");
    BROKEN_PROCESS_FAILS.store(true, Relaxed);
    BROKEN_SETUP_FAILS.store(true, Relaxed);
    for i in 0..1_000 {
        let got = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| broken.process(i)));
        let out = got.expect("a failed recovery escaped the slot");
        assert!(out.0.is_empty(), "a failed branch's sends must be dropped");
        // The branch next to it carries on.
        assert_eq!(healthy.process(i).0, [i]);
    }
    // It's retried with a backoff, not on every input.
    let setups = BROKEN_SETUPS.load(Relaxed);
    assert!(setups < 10, "setup was retried {setups} times in a burst");
    let s = stats::branch("rt_broken");
    assert!(s.failed.load(Relaxed), "the branch should show as failed");
}

#[test]
fn a_setup_that_panics_at_startup_leaves_the_branch_failed_not_the_tree() {
    STILLBORN_SETUP_FAILS.store(true, Relaxed);
    let made = std::panic::catch_unwind(|| Slot::<Stillborn>::new("rt_stillborn"));
    let mut slot = made.expect("a failing setup at startup escaped the slot");
    assert!(slot.process(1).0.is_empty());
    assert!(stats::branch("rt_stillborn").failed.load(Relaxed));
}

branch!(
    Returns,
    RETURNS_SETUPS,
    RETURNS_SETUP_FAILS,
    RETURNS_PROCESS_FAILS
);

#[test]
fn a_failed_branch_comes_back_once_setup_works_again() {
    RETURNS_SETUP_FAILS.store(true, Relaxed);
    let mut slot = Slot::<Returns>::new("rt_returns");
    for i in 0..5 {
        assert!(slot.process(i).0.is_empty());
    }
    let s = stats::branch("rt_returns");
    assert_eq!(
        s.discarded.load(Relaxed),
        5,
        "every input it missed is counted"
    );
    RETURNS_SETUP_FAILS.store(false, Relaxed);
    // Not before its retry is due...
    assert!(slot.process(5).0.is_empty());
    std::thread::sleep(crate::bonsai::SETUP_RETRY + std::time::Duration::from_millis(100));
    // ...then set up again, and this input is processed.
    assert_eq!(slot.process(6).0, [6]);
    assert!(!s.failed.load(Relaxed));
    assert_eq!(s.discarded.load(Relaxed), 6);
    assert_eq!(slot.process(7).0, [7]);
}

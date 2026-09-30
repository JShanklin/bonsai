//! Runtime regression tests. Not part of any tree: the host crate's
//! `runtime_regression_tests_pass_in_a_rendered_host_tree` test (run with
//! `cargo test -- --ignored`) renders `templates/linux/host`, copies this
//! folder in as `src/runtime_tests/`, and runs `cargo test` there, so these
//! exercise the runtime exactly as a tree compiles it.
//!
//! Tests that touch process-wide state (the logger, the panic hook, the run
//! recorder, signals) run in a child process: `support::child` re-runs this
//! test binary with one `#[ignore]`d scenario.

mod support;

mod edges;
mod framing;
mod record;
mod slot;
mod tcp;

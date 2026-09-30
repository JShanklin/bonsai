# Testing

Put decisions in plain functions and test them on your computer with
`cargo test` (`cargo local-test` in a Pi tree, which otherwise builds for the Pi). The async plumbing around them is bonsai's job and is already
tested.

## Pi / PC trees: test in place

Pull the decision out of the task:

```rust
// src/branches/watchdog.rs
fn too_hot(temp_c10: i16, limit_c10: i16) -> bool {
    temp_c10 > limit_c10
}
```

Use it in the arm (`if too_hot(temp_c10, limit_c10) { … }`), and test it at
the bottom of the same file:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_the_limit_is_fine() {
        assert!(!too_hot(300, 300));
    }

    #[test]
    fn above_the_limit_is_too_hot() {
        assert!(too_hot(301, 300));
    }
}
```

```sh
cargo local-test
```

```
test branches::watchdog::tests::above_the_limit_is_too_hot ... ok
test branches::watchdog::tests::at_the_limit_is_fine ... ok
```

## What to test

- **Decisions:** thresholds, state machines, debouncing, rate limits.
- **Encoding:** a [wire format](wire-format.md) round trip, including a
  corrupt frame being rejected.
- **Parsing:** anything that turns bytes into values.

Wiring and timing are best checked by running the tree. Use
`BONSAI_SAP_DEBUG=1` to see lag (see
[Paths](../foundations/06-paths.md#watching-the-paths)).

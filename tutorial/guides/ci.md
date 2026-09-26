# CI

Check every push automatically: formatting, lints, tests and a release
build. This is for GitHub Actions; other CI systems run the same commands.

## Pi / PC tree

`.github/workflows/ci.yml`:

```yaml
name: ci
on: [push, pull_request]

jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: aarch64-unknown-linux-gnu   # the tree builds for the Pi
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
      - run: sudo apt-get install -y gcc-aarch64-linux-gnu   # the Pi's linker
      - run: cargo fmt --check
      - run: cargo clippy -- -D warnings
      - run: cargo local-test                # tests run on the CI machine
      - run: cargo build --release           # the Pi binary
```

For a Zero W tree, use `targets: arm-unknown-linux-gnueabihf`, pick zigbuild
in `bonsai tools`, and add a step that installs zig and
`cargo install cargo-zigbuild` before the build (see
[Build tools](build-tools.md)).

## Pico tree

The same, building for the chip, plus the [logic crate's](testing.md) tests
on the runner:

```yaml
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: thumbv6m-none-eabi        # Pico 2: thumbv8m.main-none-eabihf
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
      - run: cargo fmt --check
      - run: cargo clippy --release -- -D warnings
      - run: cargo build --release
      - run: cargo build --release --no-default-features   # the lean image builds too
      - run: cd logic && cargo test --target host-tuple
```

## Also worth checking

Add a step that fails if `src/sap.rs` isn't up to date, which happens after a
hand edit to `bonsai.toml` or a branch without `bonsai sync`. It needs bonsai
installed in CI:

```yaml
      - run: cargo install --git https://github.com/JShanklin/bonsai.git
      - run: bonsai sync && git diff --exit-code src/sap.rs
```

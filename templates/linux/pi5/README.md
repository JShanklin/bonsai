# {{project-name}}

Linux userspace application for Raspberry Pi 5. Its branches decide;
bonsai's deterministic core runs them, one event at a time, on tokio. Linux
owns the hardware; add a suitable Linux GPIO, I²C, or SPI crate when the tree
needs a device. No kernel image or flashing tool is generated.

## Run and deploy

`.cargo/config.toml` makes cargo build for the Pi (64-bit Raspberry Pi OS):

```sh
cargo build --release   # → target/aarch64-unknown-linux-gnu/release/{{project-name}}
cargo run --release     # copies it to the Pi over ssh and runs it there
cargo local             # runs on this computer instead
cargo local-test        # tests on this computer
```

Set your Pi's address once in `.cargo/config.toml` (`BONSAI_PI =
"user@host"`), or for one run: `BONSAI_PI=me@mypi.local cargo run --release`.
On the Pi itself, `cargo run` runs in place. A new tree logs `INFO bonsai:
running` and waits for branches to give it something to do (`bonsai branch
add <name>`, then `bonsai rate <name> 1`). Stop it with Ctrl-C. While it
runs, `bonsai top` in this folder shows it live: each branch and edge, and
the log (over ssh, from `BONSAI_PI`).

`cargo build --release` needs a linker for the Pi's CPU. The simplest is zig:
run `bonsai tools` and pick zigbuild, which installs it once and sets this
tree up, so the usual cargo commands build for the Pi. Or use gcc, on
Debian/Ubuntu:

```sh
rustup target add aarch64-unknown-linux-gnu
sudo apt install gcc-aarch64-linux-gnu
```

On the Pi 5, GPIO, UART, I²C and SPI go through its RP1 I/O chip. Use a crate
that supports the Pi 5, such as [gpiocdev] (the GPIO character device) or a
recent [rppal] (0.22 or later). Older code that maps GPIO registers directly
doesn't work on it.

Grow it with `bonsai branch add <name>`, `bonsai message add <Name>` and
`bonsai link <from> <Message> <to>`; `bonsai list` shows the graph. Each
branch's `process` decides what to do with its inputs, with no I/O, so it can
be tested on its own.

[gpiocdev]: https://docs.rs/gpiocdev
[rppal]: https://docs.rs/rppal

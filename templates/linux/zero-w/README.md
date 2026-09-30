# {{project-name}}

Linux userspace application for Raspberry Pi Zero W. Its branches decide;
bonsai's deterministic core runs them, one event at a time, on tokio. Linux
owns the hardware; add a suitable Linux GPIO, I²C, or SPI crate when the tree
needs a device. No kernel image or flashing tool is generated.

## Run and deploy

`.cargo/config.toml` makes cargo build for the Pi (32-bit Raspberry Pi OS):

```sh
cargo build --release   # → target/arm-unknown-linux-gnueabihf/release/{{project-name}}
cargo run --release     # copies it to the Pi over ssh and runs it there
cargo local             # runs on this computer instead
cargo local-test        # tests on this computer
```

Set your Pi's address once in `.cargo/config.toml` (`BONSAI_PI =
"user@host"`), or for one run: `BONSAI_PI=me@mypi.local cargo run --release`.
On the Pi itself, `cargo run` runs in place. The built-in pulse logs
`INFO pulse: beat` twice per second. Stop it with Ctrl-C. While it runs,
`bonsai top` in this folder shows it live: each branch and edge, and the log
(over ssh, from `BONSAI_PI`).

From another computer, run `bonsai tools` and pick zigbuild: zig links for the
Zero W's ARMv6 CPU, so the usual cargo commands build for it. (Debian/Ubuntu's
`arm-linux-gnueabihf-gcc` builds for ARMv7, and its binaries crash on the
Zero W.)

On the Zero W itself, plain `cargo build --release` works (slowly).

Grow it with `bonsai branch add <name>`, `bonsai message add <Name>` and
`bonsai wire <from> <Message> <to>`; `bonsai list` shows the graph. Each
branch's `process` decides what to do with its inputs, with no I/O, so it can
be tested on its own.

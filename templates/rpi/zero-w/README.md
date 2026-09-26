# {{project-name}}

Linux userspace application for Raspberry Pi Zero W. It uses Embassy tasks and
the same trunk, branch, and nutrient commands as the microcontroller templates.
Linux owns hardware initialization; add a suitable Linux GPIO, I²C, or SPI crate
when a branch needs a device. No kernel image or flashing tool is generated.

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
On the Pi itself, `cargo run` runs in place. The built-in pulse prints
`bonsai: beat` twice per second. Stop it with Ctrl-C.

From another computer, run `bonsai tools` and pick zigbuild: zig links for the
Zero W's ARMv6 CPU, so the usual cargo commands build for it. (Debian/Ubuntu's
`arm-linux-gnueabihf-gcc` builds for ARMv7, and its binaries crash on the
Zero W.)

On the Zero W itself, plain `cargo build --release` works (slowly).

Use `bonsai branch <name>`, `bonsai feed`, `bonsai tap`, and `bonsai release`
to grow the application. The generated branch `start()` functions take the
Embassy spawner and trunk. Pass Linux device handles from `src/main.rs` when
a branch needs hardware.

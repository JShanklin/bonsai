# {{project-name}}

Linux userspace application for Raspberry Pi 5. It uses Embassy tasks and
the same trunk, branch, and nutrient commands as the microcontroller templates.
Linux owns hardware initialization; add a suitable Linux GPIO, I²C, or SPI crate
when a branch needs a device. No kernel image or flashing tool is generated.

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
On the Pi itself, `cargo run` runs in place. The built-in pulse prints
`bonsai: beat` twice per second. Stop it with Ctrl-C.

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

Use `bonsai branch <name>`, `bonsai feed`, `bonsai tap`, and `bonsai release`
to grow the application. The generated branch `start()` functions take the
Embassy spawner and trunk. Pass Linux device handles from `src/main.rs` when
a branch needs hardware.

[gpiocdev]: https://docs.rs/gpiocdev
[rppal]: https://docs.rs/rppal

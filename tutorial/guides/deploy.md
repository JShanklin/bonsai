# Deploy

Build for real, and get it onto the device.

## Raspberry Pi

Release builds are already tuned: link-time optimization, optimized for size
(for speed on a Pi 5), stripped, `panic = "abort"`. The tree's
`.cargo/config.toml` builds for the Pi, so the usual cargo commands do the
right thing:

```sh
cargo build --release   # a Pi binary in target/<pi target>/release/
cargo run --release     # build, copy to the Pi over ssh, and run it there
```

**Tell it where your Pi is.** Edit one line in `.cargo/config.toml`:

```toml
BONSAI_PI = "pi@raspberrypi.local"   # the user@host you'd give ssh
```

or override it for one run: `BONSAI_PI=me@10.0.0.7 cargo run --release`. Set
up an ssh key (`ssh-copy-id me@10.0.0.7`) so it doesn't ask for a password
every time. Ctrl-C stops the program on the Pi.

**The Pi's linker.** Building from your computer needs a linker for the Pi's
CPU:

| Pi | zig (any Linux) | gcc (Debian/Ubuntu) |
|----|-----------------|---------------------|
| Pi 5, Zero 2 W | `bonsai tools`, pick zigbuild | `rustup target add aarch64-unknown-linux-gnu` and `sudo apt install gcc-aarch64-linux-gnu` |
| Zero W | `bonsai tools`, pick zigbuild | none: Debian's 32-bit ARM linker targets a newer CPU, and its binaries crash on the Zero W |

zigbuild installs zig once and points the tree's linker at it, so
`cargo build --release` and `cargo run --release` work unchanged, and build
real ARMv6 code for the Zero W. See [Build tools](build-tools.md).

**No Pi at hand?** A [virtual Pi](virtual-pi.md) runs the same ARM build on
your computer, with its own address on your network.

**On the Pi itself,** `cargo build --release` and `cargo run --release` work as
they are, and `cargo run` runs the program in place instead of copying it.

**Run it as a service**, so it starts on boot and restarts if it stops. On the
Pi, copy the binary somewhere permanent (`cargo run` leaves it in `/tmp`):

```sh
scp target/aarch64-unknown-linux-gnu/release/greenhouse pi@raspberrypi.local:
```

and create `/etc/systemd/system/greenhouse.service`:

```ini
[Unit]
Description=greenhouse
After=network-online.target

[Service]
ExecStart=/home/pi/greenhouse
Restart=always
User=pi

[Install]
WantedBy=multi-user.target
```

```sh
sudo systemctl enable --now greenhouse
journalctl -u greenhouse -f      # its output
```

`Restart=always` matters: with `panic = "abort"`, a panic in any thread ends
the program, and systemd starts it again.

## Raspberry Pi Pico

With a debug probe connected:

```sh
cargo run --release     # build, flash, and stream defmt logs
```

**Smaller images:** panic messages take about half of a small tree's flash.
Leave them out for production:

```sh
cargo build --release --no-default-features
```

A panic still halts, and `probe-rs` still prints where it happened.

**Without a probe (RP2040):** hold BOOTSEL while plugging in the USB cable, and the
Pico appears as a drive. Convert the build with
[elf2uf2-rs](https://crates.io/crates/elf2uf2-rs)
(`elf2uf2-rs target/thumbv6m-none-eabi/release/<name>`) and copy the `.uf2`
file onto it. (You won't see logs this way.)

## ESP32-S3

```sh
cargo run --release     # espflash flashes over USB and shows the logs
```

## Before you ship

- `cargo build --release` with no warnings, and `cargo clippy` clean.
- `bonsai list` shows no wiring warnings.
- Queue caps sized from a real run (`BONSAI_SAP_DEBUG=1` / `DEFMT_LOG=debug`).
- No `.unwrap()` on anything that can fail at runtime (input, I/O, parsing).


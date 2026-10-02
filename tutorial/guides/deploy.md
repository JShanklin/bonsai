# Deploy

Build for real, and get it onto the device.

## Raspberry Pi

Release builds are already tuned: link-time optimization, optimized for size
(for speed on a Pi 5), and stripped. The tree's
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

**Moving to another Pi** (a Zero W tree onto a Pi 5, say):

```sh
bonsai retarget pi5
```

```
…
retarget `orb` from zero-w to pi5. Your code stays; these change:
  ~ Cargo.toml
  ~ .cargo/config.toml
  ~ src/main.rs
  (.cargo/config.toml keeps its [env] values and build tools; other hand edits
   there, and in the files above, are replaced by the pi5 template's)
continue? [y/N] y
build tools: sccache, zigbuild (in .cargo/config.toml)
`orb` now builds for pi5. Pins and devices (serial ports, GPIO) may
differ between boards: check the ones your branches open.
```

It swaps the build target, linker, runner and release profile for the new
board's. Branches, messages, edges and `BONSAI_PI` stay. `bonsai retarget host`
moves the tree to your own computer (plain `cargo run`), and back again later.

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
WorkingDirectory=/home/pi
Restart=always
User=pi

[Install]
WantedBy=multi-user.target
```

```sh
sudo systemctl enable --now greenhouse
journalctl -u greenhouse -f      # its log
```

The tree logs to stderr, so its lines land in the journal as they are. To
choose what's logged, add `Environment=BONSAI_LOG=warn,uplink=info` under
`[Service]`. `NO_COLOR` isn't needed: the level is only coloured on a
terminal. `WorkingDirectory=` is where [run logs](run-logs.md) go:
`/home/pi/logs/<start time>/`.

A panic in a branch doesn't stop the tree (the branch is set up again), and
a failing edge restarts by itself. `Restart=always` is for the rest: the
program being killed, running out of memory, or a bug outside a branch.

**Watch it** from your computer while it runs, over ssh like `cargo run`:

```sh
bonsai top
```

It needs nothing on the Pi but sshd. See [chapter 8](../foundations/08-watching.md).

## Before you ship

- `cargo build --release` with no warnings, and `cargo clippy` clean.
- `bonsai list` shows no warnings.
- `bonsai top` on a real run: no edge `retrying`, nothing `dropped`, no
  panics, and `waiting` near 0.
- No `.unwrap()` on anything that can fail at runtime (input, I/O, parsing).


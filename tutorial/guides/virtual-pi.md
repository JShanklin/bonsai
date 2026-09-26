# Virtual Pi

Run a Pi tree on your computer as if it were on a Raspberry Pi. You deploy
the real ARM build with `cargo run --release`, to a machine with its own
address on your network. Use it when the Pi isn't at hand, or to test a
companion computer against a simulator (SITL) and a phone before it flies.

**Needs:** a Pi tree that builds for the Pi (see [Deploy](deploy.md)), Linux
with systemd, Podman, and a clone of the bonsai repository (the setup lives in
its [`containers/`](../../containers/README.md) folder).

## How it fits together

A virtual Pi is a container running Debian, the system Raspberry Pi OS is
built on, for the board's CPU. qemu runs its ARM code on your computer, and
systemd keeps it running. It has two network links:

```
             host link: 10.89.0.2           LAN: 192.168.1.250
your computer ─────────────── virtual Pi ─────────────── phone, other computers
 (ssh, cargo run, SITL)                   (a ground station, a phone app)
```

- **The host link** is how your computer reaches it: ssh, `cargo run`, and a
  simulator running on your computer. From inside, your computer is
  `10.89.0.1`.
- **The LAN address** makes it a separate machine to every other device, with
  its own IP. Your computer can't use this one: a LAN address like this is
  hidden from the computer that hosts it, which is why there are two links.

A distrobox can't do this. It shares your computer's network and hostname, so
it never has an address of its own.

There's one virtual board per Pi, each with the real board's memory and cores:

| board | CPU | memory | cores | host link | LAN address |
|-------|-----|--------|-------|-----------|-------------|
| `pi5` | 64-bit ARM | 4 GB | 4 | `10.89.0.2` | `.250` |
| `zero-2w` | 64-bit ARM | 512 MB | 4 | `10.89.0.3` | `.251` |
| `zero-w` | 32-bit ARM | 512 MB | 1 | `10.89.0.4` | `.252` |

To change them (a Pi 5 with 8 GB, say), see
[`containers/README.md`](../../containers/README.md#changing-a-boards-specs).

## 1. Emulation

Install Podman and qemu's ARM emulation:

```sh
sudo pacman -S podman qemu-user-static qemu-user-static-binfmt   # Arch, CachyOS
sudo dnf install podman qemu-user-static                         # Fedora
sudo apt install podman qemu-user-static                         # Debian, Ubuntu
```

On Arch, `qemu-user-static-binfmt` is the part that registers the emulation
with the kernel; without it, ARM programs fail with `exec format error`.

## 2. Install a board

From your bonsai folder, with the board your tree is for:

```sh
sudo containers/install.sh pi5
```

```
virtual-pi5: building the image (emulated, so a few minutes the first time)
…
virtual-pi5: ready: ssh virtual-pi5, or BONSAI_PI=virtual-pi5 cargo run --release
virtual-pi5: host link 10.89.0.2, LAN address 192.168.1.250 on enp3s0
```

It reads your network from the default route, builds the image with your ssh
key, and hands the board to systemd. From then on it starts at boot, with
nothing to run. It also adds a `virtual-pi5` host to `~/.ssh/config` and
trusts its key, so ssh doesn't ask.

**Use a wired connection.** The script warns when the default route is Wi-Fi:
most access points drop the LAN address's traffic. To use another interface,
or another LAN address:

```sh
sudo VIRTUAL_PI_LAN_DEV=enp4s0 VIRTUAL_PI_LAN_IP=192.168.1.40 containers/install.sh pi5
```

## 3. Deploy

In the tree:

```sh
BONSAI_PI=virtual-pi5 cargo run --release
```

```
     Running `sh -c '[ "$(uname -m)" = aarch64 ] && exec "$@"
…
bonsai: beat
bonsai: beat
```

The tree's runner copies the binary with `scp` and runs it over `ssh`, so the
virtual Pi is just another ssh host. To make it the default, set
`BONSAI_PI = "virtual-pi5"` in `.cargo/config.toml`. Ctrl-C stops the program.

For a Zero W tree, pick zigbuild in `bonsai tools` first: it has no linker
otherwise (see [Deploy](deploy.md)). Then `BONSAI_PI=virtual-zero-w cargo run
--release` works the same way.

## Who reaches what

| from | to | use |
|------|----|-----|
| your computer (SITL, QGroundControl, mavlink-router) | the virtual Pi | its host link, `10.89.0.2` |
| the virtual Pi | your computer | `10.89.0.1` |
| a phone or another computer | the virtual Pi | its LAN address, `192.168.1.250` |
| a simulator on your computer, through mavlink-router | the tree | send to `10.89.0.2:14550`; the tree listens on `udpin:0.0.0.0:14560` |
| the virtual Pi, to a multicast group (`239.x.x.x`) | the LAN | goes out on `lan0` |

For example, a simulator on your computer sends MAVLink to `10.89.0.2:14550`,
and a root in the tree listens on `udpin:0.0.0.0:14550`. Check where
multicast goes:

```sh
ssh virtual-pi5 ip route get 239.1.2.3
```

```
multicast 239.1.2.3 dev lan0 src 192.168.1.250 …
```

## Day to day

```sh
systemctl status virtual-pi5          # running?
sudo systemctl restart virtual-pi5    # a fresh start (keeps /home/pi)
journalctl -u virtual-pi5             # its log, if it won't start
sudo containers/install.sh pi5        # after editing containers/: rebuild and restart
sudo containers/install.sh pi5 remove # delete it
```

Everything outside `/home/pi` resets when the board restarts. Keep your files
in `/home/pi`, add system packages to the `apt-get` line in
`containers/Containerfile`, and put services under `containers/rootfs/`, then
run the install again.

## Services

The board boots systemd, like Raspberry Pi OS, with mavlink-router already
running. A simulator on your computer sends MAVLink to `10.89.0.2:14550`, a
ground station connects with TCP to port `5760`, and a root in your tree
listening on `udpin:0.0.0.0:14560` gets the traffic. To change those, or to
start your tree at boot with its own unit file, see
[`containers/README.md`](../../containers/README.md#services).

```sh
ssh virtual-pi5 systemctl status mavlink-router
ssh virtual-pi5 journalctl -u mavlink-router -f
```

## How it differs from a real Pi

- **Speed:** emulated ARM runs several times slower than the real board.
  Memory and core count match; speed doesn't. Timing and CPU load don't carry
  over; logic and networking do.
- **Hardware:** no GPIO, I2C, SPI or camera.
- **System:** Debian, not Raspberry Pi OS. The packages and C library match;
  the Pi-only tools (`raspi-config`, the camera stack) are missing.

## If something fails

| message | fix |
|---------|-----|
| `no emulation for linux/arm64 yet` | step 1: install qemu (on Arch, both packages) |
| `sudo: effective uid is not 0` inside | rerun `install.sh`: it turns on the emulation setting `sudo` needs |
| `Unable to locate package` while building | a network hiccup; the build already uses host networking, so run it again |
| `the board didn't answer on 10.89.0.2` | `journalctl -u virtual-pi5` shows why it didn't start |
| the phone can't see it | Wi-Fi (see step 2), or a firewall on your computer blocking the LAN address |

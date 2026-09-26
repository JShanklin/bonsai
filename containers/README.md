# containers/

Virtual boards: a Raspberry Pi that runs on your computer, reached over ssh
and deployed to with `cargo run --release` like the real one. Each has its own
address on your LAN, so a phone or ground station sees a separate machine.
The walkthrough is in [the virtual Pi guide](../tutorial/guides/virtual-pi.md).

```sh
sudo containers/install.sh pi5            # set it up; systemd runs it from now on
BONSAI_PI=virtual-pi5 cargo run --release # in a Pi 5 tree
sudo containers/install.sh pi5 remove     # delete it again
```

## Boards

| board | CPU | memory | cores | host link | LAN address |
|-------|-----|--------|-------|-----------|-------------|
| `pi5` | 64-bit ARM | 4 GB | 4 | `10.89.0.2` | `.250` |
| `zero-2w` | 64-bit ARM | 512 MB | 4 | `10.89.0.3` | `.251` |
| `zero-w` | 32-bit ARM (ARMv7, runs ARMv6 builds) | 512 MB | 1 | `10.89.0.4` | `.252` |

Several can run at once. The LAN address is on your own network: `.250` means
`192.168.1.250` when your network is `192.168.1.0/24`.

**The specs are the real board's.** Each virtual board gets the memory and
cores of the board it stands in for, so a tree that runs here fits on the
real one. Memory is a hard limit: a program that goes over it is stopped,
as it would be on the board. Two things to know:

- `free` and `nproc` inside show your computer's totals. The limits still
  apply; they're enforced from outside.
- The CPU is emulated, so it runs several times slower than the real board.
  Memory and core count match; speed doesn't.

## Changing a board's specs

Each board is a file in `boards/`. To match a Pi 5 with 8 GB, edit
`boards/pi5.conf`:

```sh
MEMORY=8g    # was 4g; the Pi 5 comes in 2g, 4g, 8g and 16g
CPUS=4
```

then install it again, which applies the change and keeps `/home/pi`:

```sh
sudo containers/install.sh pi5
```

| setting | what it is |
|---------|------------|
| `MEMORY` | memory limit: `512m`, `4g`, `8g` … |
| `CPUS` | cores' worth of CPU time: `1`, `4`, or a fraction like `1.5` |
| `HOST_IP` | its address on the host link; unique per board |
| `LAN_HOST` | the last number of its LAN address; unique per board |
| `PLATFORM`, `QEMU` | the CPU it emulates; leave as they are |

To give it a different LAN address once, without editing the file:
`sudo VIRTUAL_PI_LAN_IP=192.168.1.40 containers/install.sh pi5`.

**A new board** is a new file in `boards/`: copy the closest one, and give it
its own `HOST_IP` and `LAN_HOST`.

## Services

Each board boots systemd, as Raspberry Pi OS does, so services work the same
as on the real Pi: `systemctl`, `journalctl`, `Restart=always`. Two come set
up:

| service | does |
|---------|------|
| `example` | an example to copy: logs a line from its config, `rootfs/etc/example.conf`, at each boot |
| `bonsai-multicast` | sends multicast (`239.x` groups) out on the LAN, not the host link |

```sh
ssh virtual-pi5 systemctl status example
ssh virtual-pi5 journalctl -u example
```

**Adding your own service:** put its unit file in
`rootfs/etc/systemd/system/`, and anything it needs (a config file, say)
at the same path under `rootfs/` as on the board (under `usr/`, never `lib/`
or `bin/`: on Debian those are links into `/usr`, and a real folder there
breaks the board). Rerun
`sudo containers/install.sh <board>`: every unit there is enabled, so it
starts on each boot. For example, `rootfs/etc/systemd/system/greenhouse.service`
to run a tree you've copied to `/home/pi`:

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

The same unit file works on the real Pi (see the deploy guide). Delete
`example.service` and `example.conf` once you have your own.

The `systemd-*.service.d/` folders in `rootfs/etc/systemd/system/` keep
systemd's own setup units working under CPU emulation; leave them in place.

## What's here

| file | is |
|------|----|
| `install.sh` | sets a board up (or removes it); rerun it after any change here |
| `boards/*.conf` | each board's CPU, memory, cores and addresses |
| `Containerfile` | the system inside: Debian with systemd, ssh, sudo and network tools. Add packages your Pi needs to its `apt-get` line |
| `rootfs/` | files copied onto the board as they are laid out here: service units and their configs |
| `board.container` | the Quadlet unit systemd runs each board from |
| `bonsai-host.network`, `bonsai-lan.network` | the two networks every board shares |

`install.sh` does the one-time work: it checks the CPU emulation (and turns
on the setting `sudo` needs inside), builds the image with your ssh key,
installs the units into `/etc/containers/systemd/`, and adds a
`virtual-<board>` entry to `~/.ssh/config`. After that systemd starts the
board at boot and restarts it if it stops.

Everything but `/home/pi` resets each time the board starts. Keep files
there, put system packages in the `Containerfile`, and put services and
system config under `rootfs/`.

# The bonsai tutorial

From no background at all to building real, maintainable firmware with bonsai.

## Foundations: read in order

Each chapter builds on the last. By the end you have a working multi-part
program, **greenhouse**, and you know why it's built the way it is. It runs on
an ordinary Linux or macOS computer, so you don't need hardware yet.

| # | chapter | you learn |
|---|---------|-----------|
| 1 | [Setup](foundations/01-setup.md) | install Rust, bonsai and an editor |
| 2 | [Rust essentials](foundations/02-rust-essentials.md) | just enough Rust to read and write bonsai code |
| 3 | [How firmware runs](foundations/03-how-firmware-runs.md) | tasks, `async`, why nothing may block, MCUs vs a Pi |
| 4 | [Your first tree](foundations/04-first-tree.md) | plant a tree, tour its files, run it |
| 5 | [Branches and nutrients](foundations/05-branches-and-nutrients.md) | split work into branches that pass messages |
| 6 | [Paths](foundations/06-paths.md) | pick how each message flows: broadcast, directed, state |
| 7 | [Roots](foundations/07-roots.md) | connect the tree to the outside world: sockets, ports, other programs |

## Guides: pick what you need

Short, self-contained recipes. Each one says what to add, gives you the code
to paste, and shows how to check it works. They use current, well-maintained
crates, chosen so you write little code without giving up speed or memory.

| guide | for |
|-------|-----|
| [Wire format](guides/wire-format.md) | turn messages into compact bytes and back (serde + postcard + COBS) |
| [Roots: UDP](guides/roots-udp.md) | send and receive datagrams on a network |
| [Roots: TCP](guides/roots-tcp.md) | a TCP client that reconnects by itself |
| [Roots: serial](guides/roots-serial.md) | a UART or USB-serial port |
| [Roots: MAVLink](guides/roots-mavlink.md) | ground stations and flight controllers: heartbeats, telemetry, commands |
| [Pico GPIO](guides/pico-gpio.md) | an LED and a button: handing pins to branches |
| [Testing](guides/testing.md) | unit tests on your computer, for Pi and MCU trees |
| [Deploy](guides/deploy.md) | release builds, flashing a Pico, running on a Pi |
| [Build tools](guides/build-tools.md) | sccache, mold, zigbuild and bacon: faster builds, and Pi builds with zig |
| [Virtual Pi](guides/virtual-pi.md) | an emulated Pi with its own network address, deployed to like the real one |
| [CI](guides/ci.md) | format, lint, test and build on every push |
| [Troubleshooting](guides/troubleshooting.md) | common errors and warnings, and their fixes |

The guides assume you've done the foundations, or at least chapters 4–5
(chapter 7 for the roots guides).

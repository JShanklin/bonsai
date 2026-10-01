# The bonsai tutorial

From no background at all to building real, maintainable Linux programs with
bonsai.

## Foundations: read in order

Each chapter builds on the last. By the end you have a working program,
**greenhouse**: a sensor, a watchdog and a display, talking to the network,
with tests, logs and a live view. It runs on an ordinary Linux or macOS
computer, so you don't need hardware yet.

| # | chapter | you learn |
|---|---------|-----------|
| 1 | [Setup](foundations/01-setup.md) | install Rust, bonsai and an editor |
| 2 | [Rust essentials](foundations/02-rust-essentials.md) | just enough Rust to read and write bonsai code |
| 3 | [How a tree runs](foundations/03-how-a-tree-runs.md) | branches, messages, links, edges, and the core that runs them one event at a time |
| 4 | [Your first tree](foundations/04-first-tree.md) | plant a tree, run it, tour its files |
| 5 | [Branches and messages](foundations/05-branches-and-messages.md) | split the work into branches that send each other messages |
| 6 | [Order and tests](foundations/06-order-and-tests.md) | why the order is fixed, and testing branches with no runtime |
| 7 | [Edges](foundations/07-edges.md) | connect the tree to the outside world, and test it without a network |
| 8 | [Watching a tree](foundations/08-watching.md) | logs, panics, and `bonsai top` |

## Guides: pick what you need

Short, self-contained recipes. Each one says what to add, gives you the code
to paste, and shows how to check it works.

| guide | for |
|-------|-----|
| [Edges: UDP](guides/edges-udp.md) | datagrams: unicast, replies, multicast groups |
| [Edges: TCP](guides/edges-tcp.md) | a client that reconnects by itself, or a server for many clients |
| [Edges: serial](guides/edges-serial.md) | a UART or USB-serial port: a GPS, a microcontroller |
| [Edges: your own](guides/edges-custom.md) | the `Edge` trait, for anything the built-ins don't cover |
| [Units](guides/units.md) | `Celsius`, `Meters`, `Knots`…: numbers the compiler won't mix up |
| [Binary messages](guides/binary-messages.md) | compact bytes for what crosses the network (serde + postcard + COBS) |
| [Edit and run](guides/dev-loop.md) | `bonsai dev`: rebuild and restart on every save, keeping the last good build running |
| [Testing](guides/testing.md) | branches and the whole tree, with no network |
| [Run logs](guides/run-logs.md) | a folder per run: your `record!` events, panics, errors, START and END |
| [Deploy](guides/deploy.md) | release builds, running on a Pi, as a service |
| [Build tools](guides/build-tools.md) | sccache, mold, zigbuild and bacon: faster builds, and Pi builds with zig |
| [Virtual Pi](guides/virtual-pi.md) | an emulated Pi with its own network address, deployed to like the real one |
| [CI](guides/ci.md) | format, lint, test and build on every push |
| [Troubleshooting](guides/troubleshooting.md) | common errors and warnings, and their fixes |

The guides assume you've done the foundations, or at least chapters 4–5
(chapter 7 for the edge guides).

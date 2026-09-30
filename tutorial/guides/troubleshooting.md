# Troubleshooting

## Setup

| symptom | cause | fix |
|---------|-------|-----|
| `error: no such command: generate` | cargo-generate isn't installed | `cargo install cargo-generate` |
| installing `cargo-generate` fails with "requires rustc 1.9x" | an old Rust | `rustup update`, then install again |
| `bonsai: command not found` | bonsai isn't installed, or not on your `PATH` | `cargo install --path .` inside the bonsai repo, and check that `~/.cargo/bin` is on your `PATH` |
| ``linker `aarch64-linux-gnu-gcc` not found`` (Pi tree) | no linker for the Pi's CPU | `bonsai tools` and pick zigbuild, or `sudo apt install gcc-aarch64-linux-gnu`. To run on this computer instead: `cargo local` |
| `can't find crate for core` … `target may not be installed` | Rust's library for that target is missing | `rustup target add <the target it names>` |
| `ssh: Could not resolve hostname raspberrypi.local` on `cargo run` or `bonsai top` | `BONSAI_PI` doesn't point at your Pi | set `BONSAI_PI` in `.cargo/config.toml`, or run with `BONSAI_PI=user@host`; for a tree on this computer, `bonsai top local` |

## bonsai commands

| message | means | fix |
|---------|-------|-----|
| ``run `bonsai list` inside a bonsai tree (no bonsai.toml and Cargo.toml stamp here)`` | you're not in the tree's folder | `cd` into it |
| ``no message `Readng` in src/messages.rs`` | a typo, or the message doesn't exist yet | `bonsai message add Readng …`, or fix the name |
| ``no branch or edge `sensr` `` | a typo | `bonsai list` shows the names |
| `Reading is still wired; unwire it first` | removing a message that's in use | `bonsai unwire` each wire it lists |
| ``a branch name is snake_case (a-z, 0-9, _), not a Rust keyword, and not `serial` `` | `Sensor`, `my-branch`, `type`… | `sensor`, `my_branch` |
| `` `Tick` is a name the generated code uses; pick another`` | a message named like part of the wiring (`Tick`, `Packet`, `Edge`…) | another name |
| ``[edge.net] a udp edge needs `bind` `` or `[edge.net] baud: a udp edge takes bind, to, join, iface, reply` | an edge's settings don't fit its kind | the [edge guides](../README.md#guides-pick-what-you-need) list each kind's keys |
| `a branch can't send to itself; keep that state in the branch` | `bonsai wire x M x` | keep it in the branch's struct instead |
| `this tree was grown by an older bonsai (Embassy, src/sap.rs)` | a tree from the old (Embassy) bonsai | plant a new tree and move each branch's logic into a `process` |
| `` `bonsai tap` is from the old (Embassy) bonsai; now it's …`` | a command of the old bonsai | the message names its replacement |
| ``note: no `// bonsai:input-arm` line in src/branches/x.rs`` | the marker was deleted | put `// bonsai:input-arm` back as the last line inside `match input`, and add the arm it names |

A marker moved to the end of an arm by `cargo fmt` (`} // bonsai:input-arm`)
is fine: bonsai puts it back on its own line.

## Compiling

| error | means | fix |
|-------|-------|-----|
| ``non-exhaustive patterns: `wiring::display::Input::Alarm(_)` not covered`` | something new is wired to the branch | add the arm (`Input::Alarm(alarm) => …`) |
| ``mismatched types … expected `Reading`, found `Alarm` `` at an `out.send(..)` | the branch isn't wired to send that message | `bonsai wire <branch> Alarm <to>`, or send what it is wired for |
| ``no method named `to_uplink` found for mutable reference `&mut wiring::sensor::Out` `` | the branch isn't wired to that edge | `bonsai wire <branch> uplink` |
| ``expected `Celsius`, found floating-point number`` | a unit compared or combined with a bare number | wrap the number: `Celsius(30.0)`; or take the number out: `temp.0` |
| ``cannot find … `Celsius` in this scope`` (a tree planted before units) | the units aren't imported | `pub use crate::bonsai::units::*;` at the top of `src/messages.rs` (`bonsai message add` adds it when a field uses a unit) |
| errors in `src/wiring.rs` after editing `bonsai.toml` or `src/messages.rs` by hand | the wiring is out of date | `bonsai sync` |
| `cannot find type …` in a message's fields | the type isn't imported in `src/messages.rs` | add its `use` at the top of `src/messages.rs` |

## At runtime

| log line | means | fix |
|----------|-------|-----|
| `WARN <edge>: bind 0.0.0.0:6969: Address already in use …; retrying in …` | another program (or another tree) has the port | stop it, or change the edge's port; the edge comes up once it's free |
| `WARN <edge>: connect …: Connection refused …; retrying in …` | nothing is listening there yet | start the server; the edge connects at its next try |
| `WARN <edge>: … Permission denied …` (serial) | the user can't open the port | `sudo usermod -aG dialout $USER`, then log in again |
| `WARN <edge>: isn't keeping up; dropping what's sent to it` | branches send the edge faster than it can go out | send less often, or check the link; `bonsai top` counts the drops |
| `ERROR <branch>: panicked at …`, then `WARN <branch>: set up again after a panic` | a bug in `process`; the branch was reset | fix it; `RUST_BACKTRACE=1` shows the backtrace |
| `ERROR bonsai: one event set off over 10000 messages; dropping the … left` | branches send to each other without end | `bonsai list` warns about the loop; make one of its sends conditional |
| `WARN bonsai: top: can't listen on 127.0.0.1:7777 …` | another tree already serves `bonsai top` there | `BONSAI_TOP=7778` for this one (and `bonsai top --port 7778`) |
| `bonsai: BONSAI_LOG: didn't understand …` | a typo in `BONSAI_LOG` | levels are `off`, `error`, `warn`, `info`, `debug` |

| symptom | likely cause | fix |
|---------|--------------|-----|
| a branch never runs | nothing is wired to it and it has no `rate` | `bonsai list` warns about it |
| the tree stops responding | something in a branch or an edge blocks: a busy loop, `std::thread::sleep`, blocking I/O | branches must not wait; move the waiting into an edge, and in an edge use async I/O or `tokio::task::spawn_blocking` |
| `bonsai top` shows `waiting` climbing | the core can't keep up | its `max µs` column shows which branch is slow |
| nothing arrives over multicast | the network drops it (common on Wi-Fi) | see [UDP: multicast on a Pi](edges-udp.md#multicast-on-a-pi) |

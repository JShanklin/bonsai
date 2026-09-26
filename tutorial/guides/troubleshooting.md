# Troubleshooting

## Setup

| symptom | fix |
|---------|-----|
| `error: no such command: generate` | `cargo install cargo-generate` |
| installing `cargo-generate` fails with "requires rustc 1.9x" | `rustup update`, then install again |
| `bonsai: command not found` | `cargo install --path .` inside the bonsai repo, and check that `~/.cargo/bin` is on your `PATH` |
| no completions for an ESP32 tree in Zed | run `bonsai ide` in the tree |
| ``linker `aarch64-linux-gnu-gcc` not found`` (Pi tree) | no linker for the Pi's CPU | `bonsai tools` and pick zigbuild, or `sudo apt install gcc-aarch64-linux-gnu`. To run on this computer instead: `cargo local` |
| `can't find crate for core` … `target may not be installed` | Rust's library for that target is missing | `rustup target add <the target it names>` |
| `ssh: Could not resolve hostname raspberrypi.local` on `cargo run` | `BONSAI_PI` doesn't point at your Pi | set `BONSAI_PI` in `.cargo/config.toml`, or run `BONSAI_PI=user@host cargo run --release` |
| ``linker `xtensa-esp32s3-elf-gcc` not found`` (ESP32-S3) | the linker espup installed isn't on your `PATH` | `. ~/export-esp.sh`, and add that line to `~/.bashrc` / `~/.zshrc` (fish: `source ~/export-esp.sh` in `~/.config/fish/config.fish`). No `~/export-esp.sh`? Run `espup install` again |

## bonsai commands

| message | means | fix |
|---------|-------|-----|
| `run this inside a bonsai tree` | you're not in the project folder | `cd` into it |
| `marker … not found` | a `// bonsai:…` comment was deleted or edited | put it back (a fresh tree shows where it goes) |
| `this tree predates per-nutrient paths` | a tree from a very old bonsai | `bonsai regrow`, or migrate by hand (see the README) |
| `--roots needs a std tree` | roots run OS threads, which MCUs don't have | on an MCU, use a branch that owns the peripheral |
| `refusing to starve Beat` | the built-in pulse needs it | leave it |
| `` `x` already taps `Y` `` / `already releases` | it's already wired | nothing to do; `bonsai list` shows the wiring |

## Compiling

| error | means | fix |
|-------|-------|-----|
| `missing fields … in initializer of Nutrient` | `bonsai release` left `/* TODO: fields */` | fill in the fields |
| `non-exhaustive patterns` in `src/sap.rs` | you changed `Nutrient` by hand | `bonsai sync` |
| warning: `unused … must be used … a directed release` | a `sap.release(..)` without `.await` | add `.await` |
| `cannot find type …` in a nutrient's fields | the type isn't imported in `src/trunk.rs` | add the `use` there; the sap reads trunk.rs's imports |

## At runtime

| symptom | likely cause | fix |
|---------|--------------|-----|
| everything freezes | a task blocks: a busy loop, `std::thread::sleep`, or blocking I/O on the executor | await instead, or move the I/O into a `--roots` branch |
| a branch never receives | it doesn't tap that nutrient, or nothing releases it | `bonsai list` |
| panic: `sap: more taps than bonsai counted` | the wiring changed by hand since the last sync | `bonsai sync` |
| messages are lost | a broadcast tapper falls behind | `BONSAI_SAP_DEBUG=1` (Pi) or `DEFMT_LOG=debug` (MCU) shows who; raise that path's `--cap`, or speed up the reader |
| a sender stalls | a directed path is full, and its reader is slow or stuck | fix the reader, or use `sap.try_release(..)` where waiting isn't acceptable |
| the program exits on a Pi | a panic, with `panic = "abort"` | read the log (`journalctl -u <name>`), fix the panic; run it under systemd with `Restart=always` |

## bonsai's wiring warnings

| warning | fix |
|---------|-----|
| directed … tapped by A and B | make it broadcast (`bonsai path X broadcast`), or give each reader its own nutrient |
| deadlock risk | use `sap.try_release(..)` there, or make the path broadcast |
| taps and releases … feeds itself | release only under a condition, inside the arm |
| `Big` is ≈ N B … its path holds ≈ M B | lower `--cap`, or send a `Box` (Pi) or a buffer index (MCU) instead of the bytes |

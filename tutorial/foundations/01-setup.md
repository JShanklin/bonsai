# 1. Setup

You need a terminal, the Rust toolchain, bonsai and an editor. It takes about
15 minutes.

## A terminal

- **Linux:** your terminal app.
- **macOS:** Terminal (in Applications → Utilities).
- **Windows:** install [WSL](https://learn.microsoft.com/windows/wsl/install),
  open "Ubuntu", and follow the Linux steps from there.

Commands in this tutorial go in the terminal. `#` starts a comment; don't
type it.

## Rust

Install `rustup`, which installs and updates Rust:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Accept the defaults, then open a new terminal and check it worked:

```sh
rustc --version   # rustc 1.9x.y …
cargo --version   # cargo is Rust's build tool and package manager
```

Already had Rust? Update it. The tools below need a recent version:

```sh
rustup update
```

## bonsai

bonsai creates projects with `cargo-generate`, so install that first. Then
install bonsai from its repository:

```sh
cargo install cargo-generate
git clone https://github.com/JShanklin/bonsai.git
cd bonsai
cargo install --path .
bonsai help        # prints every command
```

These take a few minutes to compile the first time.

## An editor

Any editor works. Two good choices, both with **rust-analyzer**, which gives
you completions, inline errors and go-to-definition:

- [VS Code](https://code.visualstudio.com/) with the *rust-analyzer*
  extension.
- [Zed](https://zed.dev/), which has Rust support built in.

## Hardware (later)

Chapters 1–6 need no hardware. For the hardware guides:

| board | good for | extra setup |
|-------|----------|-------------|
| Raspberry Pi Zero W / Zero 2 W / Pi 5 | Linux: networking, files, USB devices | a linker for the Pi (zig, via `bonsai tools`); see the [deploy guide](../guides/deploy.md) |

Next: [Rust essentials](02-rust-essentials.md).

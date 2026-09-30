# {{project-name}}

Linux userspace application for this computer. Its branches decide;
bonsai's deterministic core runs them, one event at a time, on tokio. Linux
owns the hardware; add a suitable Linux crate when the tree needs a device.

## Run

This tree builds for the computer it's on:

```sh
cargo run               # build and run it here
cargo test              # its tests
cargo build --release   # → target/release/{{project-name}}
```

A new tree logs `INFO bonsai: running` and waits for branches to give it
something to do (`bonsai branch add <name>`, then `bonsai rate <name> 1`).
Stop it with Ctrl-C. While it runs, `bonsai top` shows it live: each branch
and edge, and the log.

To move the tree to a Raspberry Pi, run `bonsai retarget <board>` (`pi5`,
`zero-2w` or `zero-w`): it switches the build target and adds the runner that
copies the binary to the Pi over ssh.

Grow it with `bonsai branch add <name>`, `bonsai message add <Name>` and
`bonsai link <from> <Message> <to>`; `bonsai list` shows the graph. Each
branch's `process` decides what to do with its inputs, with no I/O, so it can
be tested on its own.

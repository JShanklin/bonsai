# {{project-name}}

Linux userspace application for this computer. It uses Embassy tasks, grown with
bonsai's trunk, branch, and nutrient commands. Linux owns hardware
initialization; add a suitable Linux crate when a branch needs a device.

## Run

This tree builds for the computer it's on:

```sh
cargo run               # build and run it here
cargo test              # its tests
cargo build --release   # → target/release/{{project-name}}
```

The built-in pulse prints `bonsai: beat` twice per second. Stop it with Ctrl-C.

To move the tree to a Raspberry Pi, run `bonsai retarget <board>` (`pi5`,
`zero-2w` or `zero-w`): it switches the build target and adds the runner that
copies the binary to the Pi over ssh.

Use `bonsai branch <name>`, `bonsai feed`, `bonsai tap`, and `bonsai release`
to grow the application. The generated branch `start()` functions take the
Embassy spawner and trunk. Pass device handles from `src/main.rs` when a branch
needs hardware.

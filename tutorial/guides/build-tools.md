# Build tools

Faster builds, and Pi builds without a cross toolchain. Pick the tools when
you plant a tree (the wizard's Build tools step), or later in a tree:

```sh
bonsai tools
```

Space picks a tool, Enter confirms. A tool that isn't installed yet installs
once: system packages with `sudo` (pacman, dnf or apt), the rest with
`cargo install`. The settings go in the tree's own `.cargo/config.toml`, so
each tree records what it uses.

| tool | what it does | trees | installs |
|------|--------------|-------|----------|
| **sccache** | reuses compiled crates across builds and trees | all | `cargo install sccache` |
| **mold** | a fast linker, for builds that run on this computer (`cargo local`) | Pi | `mold`, `clang` packages |
| **zigbuild** | links Pi builds with zig: no cross toolchain, right code for every Pi | Pi | `zig` package, `cargo install cargo-zigbuild` |
| **bacon** | rebuilds and shows errors each time you save | all | `cargo install bacon` |

**Without the menu:** name the tools. Exactly those are on, the rest off:

```sh
bonsai tools sccache zigbuild bacon
```

```
build tools: sccache, zigbuild, bacon (in .cargo/config.toml)
```

## What each one writes

With sccache, mold and zigbuild on, a Pi 5 tree's `.cargo/config.toml` gains:

```toml
[build]
target = "aarch64-unknown-linux-gnu"
# sccache reuses compiled crates (`bonsai tools`).
rustc-wrapper = "sccache"

[target.aarch64-unknown-linux-gnu]
# zig links the Pi's build (zigbuild, `bonsai tools`).
linker = ".cargo/zig-cc"
…

[target.x86_64-unknown-linux-gnu]
# mold links builds for this computer (`bonsai tools`).
linker = "clang"
rustflags = ["-C", "link-arg=-fuse-ld=mold"]
```

- **zigbuild** also writes `.cargo/zig-cc`, a short script that hands
  linking to zig. With it, the usual `cargo build --release` and
  `cargo run --release` build for the Pi with no gcc for the Pi's CPU
  installed, and for the Zero W they build real ARMv6 code.
- **mold** only touches builds for this computer, so it speeds up
  `cargo local` and `cargo local-test`, not the Pi's builds.
- **bacon** writes nothing: run `bacon` in the tree and leave it open.
- **sccache** keeps its cache in your home folder, shared by every tree. After
  a `cargo clean`, a rebuild comes from the cache instead of compiling again.

Unpicking a tool in `bonsai tools` takes its settings out again. With none
picked, `.cargo/config.toml` is exactly as the template made it.

## Microcontroller trees

Pico and ESP32 trees are offered sccache and bacon only. Their builds link
with the chip's own linker, so mold and zig don't apply.

## If something fails

| message | fix |
|---------|-----|
| `zig isn't packaged for Debian or Ubuntu` | get zig from https://ziglang.org/download/, put it on your `PATH`, run `bonsai tools` again |
| `<tool>: still not installed, so the tree won't use it` | the install above it failed (a declined `sudo` prompt, no network); fix that and run `bonsai tools` again |
| ``linking with `…/.cargo/zig-cc` failed`` … `cargo-zigbuild: not found` | the tree uses zigbuild on a computer without it: run `bonsai tools` there |

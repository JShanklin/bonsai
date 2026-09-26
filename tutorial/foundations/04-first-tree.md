# 4. Your first tree

You'll plant **greenhouse**, a monitor that you'll grow over the next
chapters. It uses the Raspberry Pi Zero 2 W template, which builds an
ordinary program, so it also runs on your computer. No hardware needed.

## Plant it

In a folder where you keep projects:

```sh
bonsai
```

A menu opens. Use the arrow keys (or `j`/`k`) and Enter:

1. **MCU:** `rpi`
2. **Chip:** `bcm2710a1`
3. **Board:** `zero-2w`
4. **Where:** `new folder`
5. **Project name:** type `greenhouse`, then Enter.

bonsai hands off to `cargo-generate` and creates a `greenhouse/` folder
(already a git repository).

Have a Pi 5? Pick `bcm2712` and `pi5` instead. Everything in the tutorial works
the same.

**Already made the folder?** Run `bonsai init` inside it (or pick `here` at
step 4). The tree is planted right there, with no subfolder, and the name
starts out as the folder's. Nothing is overwritten: if a file the tree needs,
like `Cargo.toml`, is already there, bonsai stops and lists it. It also leaves
git alone, so run `git init` yourself if the folder isn't a repository yet.

```sh
mkdir greenhouse && cd greenhouse
bonsai init
```

In an empty folder, the wizard picks `here` for you anyway.

## Run it

```sh
cd greenhouse
cargo local
```

(Skip the `cd` if you used `bonsai init`: you're already there.)

`cargo local` runs the tree on your computer. A Pi tree is set up to build for
the Pi, so plain `cargo run` would copy it to a Pi and run it there. You'll
use that once you have one (see the [deploy guide](../guides/deploy.md)).
Until then, `cargo local` it is.

The first build takes a minute. Then:

```
bonsai: beat
bonsai: beat
```

twice a second. That's the **pulse**: a built-in heartbeat proving that the
executor is running and messages are flowing. Stop it with Ctrl-C.

## What's inside

```
greenhouse/
├── .cargo/
│   └── config.toml  # builds for the Pi; `cargo local` runs here instead
├── Cargo.toml       # name, dependencies, build settings
├── bonsai.toml      # how each nutrient flows (bonsai edits this for you)
└── src/
    ├── main.rs      # the trunk: starts the pulse, then every branch
    ├── trunk.rs     # the Nutrient enum: your tree's vocabulary
    ├── sap.rs       # GENERATED channels; never edit
    ├── pulse.rs     # the heartbeat
    └── branches/
        └── mod.rs   # the list of branches (empty for now)
```

Open `src/trunk.rs`. The `Nutrient` enum has one variant, `Beat`, which the
pulse sends and receives. The comment `// bonsai:nutrient` marks where
bonsai adds new ones. **Keep every `// bonsai:…` comment:** they're where
bonsai edits your files.

## Look at the tree

```sh
bonsai list
```

```
tree: greenhouse  (rpi / bcm2710a1 / zero-2w, target aarch64-unknown-linux-gnu)
sap: paths — one channel per nutrient
  Beat  broadcast  cap 2    pulse → pulse
  ≈ 0 B of queued payload across 2 slots
branches: none
```

The middle part is the **flow graph**: each nutrient, how it flows, who sends
it → who receives it. Right now the pulse talks to itself.

## Save your progress

bonsai edits files for you, so commit before and after each step. `git diff`
then shows exactly what each command changed:

```sh
git add -A && git commit -m "Plant greenhouse"
```

Next: [Branches and nutrients](05-branches-and-nutrients.md).

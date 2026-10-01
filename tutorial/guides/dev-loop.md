# Edit and run

`bonsai dev` runs the tree on your computer and, every time you save,
rebuilds it and restarts it. You keep editing; it keeps the latest
working build running.

**Needs:** [chapter 5](../foundations/05-branches-and-messages.md).

```sh
bonsai dev
```

```
bonsai dev: watching src/, bonsai.toml, Cargo.toml and .cargo/config.toml (Ctrl-C to stop)
bonsai dev: building…
bonsai dev: started (pid 17259)
00:25:37.682Z  INFO bonsai: running
```

Save a change and it builds again. Only a build that succeeds replaces the
running tree: the old one is stopped first, with SIGTERM, as systemd stops
a service (so its run logs end with `END SIGTERM`), then the new one
starts:

```
bonsai dev: changed: src/main.rs
bonsai dev: building…
bonsai dev: stopping pid 16473 for the new build
00:25:22.062Z  INFO bonsai: stopping (SIGTERM)
bonsai dev: started (pid 16737)
```

## When the build fails

The compiler's errors are printed as usual, and the tree that was running
keeps running, so a half-finished edit doesn't take it down:

```
bonsai dev: changed: src/main.rs
bonsai dev: building…
error: on purpose
bonsai dev: build failed (above); the previous build is still running
```

Fix it and save again. If the tree had stopped by itself in the meantime,
it says `nothing is running`, and the next good build starts it.

## When bonsai.toml changes

`bonsai dev` checks the graph before it builds. With errors in
`bonsai.toml`, it lists them and builds nothing. When the generated code
is out of date (you edited `bonsai.toml` by hand), it says so, and also
builds nothing:

```
bonsai dev: changed: bonsai.toml
bonsai dev: not building: generated code is out of date (src/links.rs); run `bonsai sync`, or `bonsai dev --sync` to sync on every change
```

It never writes your files unless you ask: run `bonsai sync` yourself (the
next build follows), or start it as `bonsai dev --sync` to sync on every
change. The bonsai commands (`link`, `rate`, …) sync anyway.

## What it watches

Your sources: `src/**/*.rs`, `bonsai.toml`, `Cargo.toml`,
`.cargo/config.toml` and `build.rs`. Never `target/`, the run logs, or
`Cargo.lock`, so a build or a running tree can't set off another build.
Saves that come together (save-all, a formatter) are taken as one: it
waits until nothing has changed for 0.3 s. A save during a build starts
another build before anything is restarted.

## Stopping

Ctrl-C stops the tree (it gets Ctrl-C, as if you'd run it yourself), then
`bonsai dev`. During a build, the build is stopped too, whatever it's doing:
cargo and everything it started get SIGTERM, then SIGKILL after 2 s.

The tree runs as a process group of its own, and everything it starts
(a helper it spawns, a shell command) is in that group. Stopping it, for a
restart or for Ctrl-C, stops the whole group: Ctrl-C (or SIGTERM, for a
restart) to every process in it, then SIGKILL after 5 s to whatever's
left, and only then does a new build start. When the tree exits by
itself, whatever it left running in its group goes the same way.

That includes what leaves the group: a child that calls `setsid` is still
the tree's child, and a daemon that double-forks (its parent exits, so it
would be left to the system) comes to `bonsai dev` instead, which runs as
a subreaper for exactly that. Each is matched by its process id *and*
start time, and signalled through a pidfd, so no other process is ever
hit, even if an id is reused.

Where Linux allows it (Debian, Raspberry Pi OS, Fedora, Arch; root
anywhere), the tree also runs in a PID namespace of its own, under a small
wrapper (`bonsai __dev-run`, which you'll see in `ps`). When a namespace's
first process ends, the kernel kills everything inside, so nothing the tree
started can be left behind, whatever it did to get away (`setsid`, a double
fork, an empty environment), and even if `bonsai dev`, the wrapper and
everything else of bonsai's are killed outright. When only `bonsai dev`
is killed, the tree still gets SIGTERM and 5 s first. Inside, the tree is
pid 2 to itself; its run logs and `bonsai dev` show the id `ps` shows.

Where it isn't allowed (Ubuntu 24.04 and later keep ordinary users from
making namespaces), `bonsai dev` says so when it starts, and a helper
(`bonsai __dev-guardian`) does the job instead: if `bonsai dev` is killed,
it stops the tree and everything it started, SIGTERM and then SIGKILL after
5 s, finding them by what it last saw and by `BONSAI_DEV_RUN`, set in the
environment of everything the tree starts. Two things can get past it: a
process that clears its environment and leaves in the moment before
`bonsai dev` dies, and anything at all if the helper is killed outright
too. `BONSAI_DEV_NO_NAMESPACE=1` asks for this way where namespaces work.

A daemon a build leaves running (sccache's server) is never stopped.

Arguments after `--` go to the tree (`bonsai dev -- <args>`), as
`cargo run -- <args>` would pass them.

## On a Pi tree

`bonsai dev` builds for this computer, like `cargo local`, whatever the
tree's board. To run on the Pi, use `cargo run` as before.

`bacon` ([build tools](build-tools.md)) is the other way to keep a build
going: it shows the compiler's errors as you save, and runs nothing.

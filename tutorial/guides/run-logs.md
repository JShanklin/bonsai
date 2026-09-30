# Run logs

Keep a record of each run: when it started, the events you choose, what
went wrong, and when and why it stopped. Each run gets its own folder,
named by the local time it started, with a file for each kind of line.

**Needs:** [chapter 5](../foundations/05-branches-and-messages.md).

## What a run leaves behind

A tree launched at 20:07:45 and stopped with Ctrl-C three seconds later:

```
logs/2026-09-30_20-07-45/
├── events.log
└── panics.log
```

`events.log`:

```
2026-09-30 20:07:45.933 START greenhouse 0.1.0 on vm (pid 6993, UTC+02:00)
2026-09-30 20:07:46.436 sensor: launch: altitude 120 m
2026-09-30 20:07:48.436 sensor: launch: altitude 120 m
2026-09-30 20:07:49.233 END Ctrl-C, after 3s
```

`panics.log`:

```
2026-09-30 20:07:45.933 START greenhouse 0.1.0 on vm (pid 6993, UTC+02:00)
2026-09-30 20:07:47.436 sensor: panicked at src/branches/sensor.rs:35: sensor unplugged
2026-09-30 20:07:49.233 END Ctrl-C, after 3s
```

Every file opens with a **START** line (the tree, its version, the
computer, the process id and the time zone) and closes with an **END**
line: `Ctrl-C`, or `SIGTERM` when systemd stops the service, and how long
it ran. Times are local, to the millisecond.

## Log an event

`record!` works like `info!`: the line goes to the normal log (and
`bonsai top`), and into `events.log`, tagged with the branch or edge that
wrote it:

```rust
Input::Tick => {
    self.ticks += 1;
    if self.ticks == 2 {
        record!("launch: altitude {}", Meters(120.0));
    }
    if self.ticks == 4 {
        panic!("sensor unplugged");
    }
}
```

(This sensor panics on its fourth tick, to show `panics.log`; bonsai sets
it up again, so it counts from 0 and records the launch a second time.)

Use `record!` for what you'll want to find later: a launch, a mode
change, a limit crossed. Everyday detail stays in `info!`/`debug!`.

## Choose what's kept

`[record]` in `bonsai.toml` says where the folders go and which files each
run gets:

```toml
[record]
dir = "logs"        # relative to where the tree runs
events = true       # record!("…") from a branch or an edge
panics = true       # a branch or an edge panicked
errors = false      # every error!/warn! line
edges = false       # edges coming up and going down
```

Switch one with `bonsai record`, which keeps the comments:

```sh
bonsai record errors on
```

```
record: events, panics, errors → logs/
updated src/links.rs
```

| file | holds |
|------|-------|
| `events.log` | every `record!(..)` |
| `panics.log` | each panic in a branch or an edge, with where and why |
| `errors.log` | every `error!` and `warn!` line, like `hub: WARN connect 127.0.0.1:1: Connection refused (os error 111); retrying in 100ms` |
| `edges.log` | each edge coming `up`, or going `down` and why |

`bonsai record` alone shows what's kept; `bonsai list` shows it too.
`bonsai record dir /var/log/greenhouse` moves the folders.

## A run that didn't end

A tree that was killed (`kill -9`) or lost power writes no END line. The
next run notices, and says so under its START line:

```
2026-09-30 14:05:30.641 START greenhouse 0.1.0 on vm (pid 5181, UTC-04:00)
2026-09-30 14:05:30.641 previous run 2026-09-30_18-05-28 has no END line: it was killed or lost power
```

## Where the folders go

`dir` is relative to the folder the tree runs in: the tree's own folder
with `cargo local`, and the service's `WorkingDirectory=` when it runs as
a service ([Deploy](deploy.md)). A new tree's `.gitignore` leaves `logs/`
out of git.

For one run, `BONSAI_RECORD` overrides it:

```sh
BONSAI_RECORD=off cargo local           # keep nothing this time
BONSAI_RECORD=/tmp/runs cargo local     # folders go here instead
```

A full disk or a read-only folder never stops the tree: bonsai says so
once on stderr and stops writing that file.

## Older trees

A tree planted before run logs has no `[record]` table, so it keeps
nothing; `record!` still logs like `info!`. `bonsai record events on` adds
the table.

//! `bonsai dev`: run the tree on this computer, and rebuild and restart it
//! whenever its sources change.
//!
//! On a change (edits are let settle first), the graph is checked, the tree
//! built, and only a build that succeeded replaces the running program: it's
//! stopped with SIGTERM (as systemd would; SIGKILL after `STOP_WAIT`) and the
//! new one started. A failed build leaves the previous one running. Generated
//! files are synced only with `--sync`; otherwise a stale tree isn't built.
//! What's watched is the tree's sources (`src/**/*.rs`, bonsai.toml,
//! Cargo.toml, .cargo/config.toml, build.rs), never target/, logs or
//! Cargo.lock, and what `--sync` writes isn't a change. Ctrl-C stops the
//! program (it gets SIGINT, as if run directly) and then `bonsai dev`; the
//! program runs in its own process group and, on Linux, is ended if `bonsai
//! dev` itself is killed, so nothing is left running.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

/// How often the sources are looked at.
const POLL: Duration = Duration::from_millis(200);
/// Edits are taken together until the sources stay unchanged this long.
pub const SETTLE: Duration = Duration::from_millis(300);
/// How long a program gets to stop before it's killed.
pub const STOP_WAIT: Duration = Duration::from_secs(5);

/// The state of the watched files: path → (modified, length).
pub type Snapshot = BTreeMap<PathBuf, (Option<SystemTime>, u64)>;

/// Whether a path (relative to the tree) is one of its sources.
pub fn watched(rel: &Path) -> bool {
    let s = rel.to_string_lossy();
    matches!(
        s.as_ref(),
        "bonsai.toml" | "Cargo.toml" | ".cargo/config.toml" | "build.rs"
    ) || (s.starts_with("src/") && s.ends_with(".rs") && !s.contains("/."))
}

/// The tree's sources as they are now.
pub fn snapshot(root: &Path) -> Snapshot {
    fn walk(root: &Path, dir: &Path, out: &mut Snapshot) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let path = e.path();
            let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            if path.is_dir() {
                walk(root, &path, out);
            } else if watched(&rel)
                && let Ok(m) = e.metadata()
            {
                out.insert(rel, (m.modified().ok(), m.len()));
            }
        }
    }
    let mut out = Snapshot::new();
    walk(root, &root.join("src"), &mut out);
    for f in [
        "bonsai.toml",
        "Cargo.toml",
        ".cargo/config.toml",
        "build.rs",
    ] {
        if let Ok(m) = std::fs::metadata(root.join(f)) {
            out.insert(PathBuf::from(f), (m.modified().ok(), m.len()));
        }
    }
    out
}

/// The files that differ between two snapshots (changed, added or gone).
pub fn changed(before: &Snapshot, after: &Snapshot) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = after
        .iter()
        .filter(|(p, v)| before.get(*p) != Some(v))
        .map(|(p, _)| p.clone())
        .collect();
    out.extend(before.keys().filter(|p| !after.contains_key(*p)).cloned());
    out.sort();
    out
}

/// The program a `cargo build --message-format=json-render-diagnostics`
/// built: the last binary artifact's `executable`.
pub fn executable(cargo_json: &str) -> Option<PathBuf> {
    cargo_json
        .lines()
        .filter(|l| l.contains("\"reason\":\"compiler-artifact\""))
        .filter_map(|l| {
            let at = l.find("\"executable\":\"")? + "\"executable\":\"".len();
            let end = l[at..].find('"')?;
            Some(PathBuf::from(l[at..at + end].replace("\\\\", "\\")))
        })
        .next_back()
}

/// What happened, for the person watching (and the tests).
#[derive(Debug, Clone, PartialEq)]
pub enum Note {
    Watching,
    Changed(Vec<PathBuf>),
    Synced(String),
    /// The graph can't be built from; nothing was built.
    Refused(Vec<String>),
    /// Generated files are out of date and `--sync` wasn't asked for.
    Stale(Vec<String>),
    Building,
    /// The build failed; whether the previous program is still running.
    BuildFailed {
        still_running: bool,
    },
    Started(u32),
    Stopping(u32),
    /// It didn't stop within `STOP_WAIT`, and was killed.
    Killed(u32),
    /// The program ended by itself.
    Exited(String),
    Done,
}

impl Note {
    fn text(&self) -> String {
        match self {
            Note::Watching => {
                "watching src/, bonsai.toml, Cargo.toml and .cargo/config.toml (Ctrl-C to stop)"
                    .to_string()
            }
            Note::Changed(files) => {
                let names: Vec<String> = files.iter().map(|f| f.display().to_string()).collect();
                format!("changed: {}", names.join(", "))
            }
            Note::Synced(what) => format!("synced: updated {what}"),
            Note::Refused(errors) => format!(
                "not building: the tree has errors\n{}",
                errors
                    .iter()
                    .map(|e| format!("  error: {e}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
            Note::Stale(files) => format!(
                "not building: generated code is out of date ({}); run `bonsai sync`, or `bonsai dev --sync` to sync on every change",
                files.join(", ")
            ),
            Note::Building => "building…".to_string(),
            Note::BuildFailed {
                still_running: true,
            } => "build failed (above); the previous build is still running".to_string(),
            Note::BuildFailed {
                still_running: false,
            } => "build failed (above); nothing is running".to_string(),
            Note::Started(pid) => format!("started (pid {pid})"),
            Note::Stopping(pid) => format!("stopping pid {pid} for the new build"),
            Note::Killed(pid) => format!(
                "pid {pid} didn't stop within {}s; killed it",
                STOP_WAIT.as_secs()
            ),
            Note::Exited(how) => format!("the tree exited ({how}); waiting for a change"),
            Note::Done => "stopped".to_string(),
        }
    }
}

/// How `bonsai dev` runs.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Run `bonsai sync` on every change.
    pub sync: bool,
    /// Arguments for the program.
    pub args: Vec<String>,
}

/// The `cargo build` arguments for this computer: a Pi tree builds for the
/// host (as `cargo local` does), a host tree as it is.
fn build_args(root: &Path) -> Vec<String> {
    let mut args = vec![
        "build".to_string(),
        "--message-format=json-render-diagnostics".to_string(),
    ];
    let config = std::fs::read_to_string(root.join(".cargo/config.toml")).unwrap_or_default();
    if crate::parse_target(&config).is_some() {
        args.extend(["--target".to_string(), "host-tuple".to_string()]);
    }
    args
}

/// Build the tree; the program, or None when the build failed (cargo's
/// errors are on stderr already).
fn build(root: &Path, stop: &AtomicBool) -> Option<PathBuf> {
    let mut child = Command::new("cargo")
        .args(build_args(root))
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .ok()?;
    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        use std::io::Read;
        let _ = stdout.read_to_string(&mut out);
    }
    let status = child.wait().ok()?;
    if stop.load(Ordering::SeqCst) || !status.success() {
        return None;
    }
    executable(&out)
}

/// Start the program in its own process group (so Ctrl-C reaches `bonsai
/// dev`, which stops it in order) that ends with `bonsai dev` on Linux.
fn start(program: &Path, root: &Path, args: &[String]) -> std::io::Result<Child> {
    use std::os::unix::process::CommandExt;
    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(root).process_group(0);
    #[cfg(target_os = "linux")]
    // SAFETY: prctl is async-signal-safe, and nothing else runs between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    cmd.spawn()
}

/// Ask `child` to stop with `signal`; kill it after `STOP_WAIT`. True when
/// it had to be killed.
fn stop_child(child: &mut Child, signal: i32) -> bool {
    // SAFETY: kill on a pid we started and haven't reaped.
    unsafe {
        libc::kill(child.id() as i32, signal);
    }
    let until = Instant::now() + STOP_WAIT;
    while Instant::now() < until {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    true
}

/// Wait for the sources to change from `base` and then settle; the new
/// snapshot, or None when told to stop. Meanwhile, a program that ends by
/// itself is reported.
fn next_change(
    root: &Path,
    base: &Snapshot,
    running: &mut Option<Child>,
    stop: &AtomicBool,
    note: &mut dyn FnMut(Note),
) -> Option<Snapshot> {
    loop {
        if stop.load(Ordering::SeqCst) {
            return None;
        }
        if let Some(child) = running
            && let Ok(Some(status)) = child.try_wait()
        {
            note(Note::Exited(status.to_string()));
            *running = None;
        }
        let now = snapshot(root);
        if now != *base {
            // Let a burst of edits (a save-all, a formatter) finish.
            let mut last = now;
            let mut quiet_since = Instant::now();
            while quiet_since.elapsed() < SETTLE {
                if stop.load(Ordering::SeqCst) {
                    return None;
                }
                std::thread::sleep(POLL.min(SETTLE / 3));
                let again = snapshot(root);
                if again != last {
                    last = again;
                    quiet_since = Instant::now();
                }
            }
            return Some(last);
        }
        std::thread::sleep(POLL);
    }
}

/// The loop: build and run, then rebuild and restart on every change, until
/// `stop` is set. `note` hears what happens.
pub fn run(root: &Path, opts: &Options, stop: &AtomicBool, note: &mut dyn FnMut(Note)) {
    let mut running: Option<Child> = None;
    let mut base = snapshot(root);
    note(Note::Watching);
    let mut first = true;
    loop {
        if !first {
            match next_change(root, &base, &mut running, stop, note) {
                None => break,
                Some(now) => {
                    note(Note::Changed(changed(&base, &now)));
                    base = now;
                }
            }
        }
        first = false;
        // The graph first: errors, then generated files.
        match crate::sync::plan(root) {
            Err(crate::sync::Refused::Config(e)) => {
                note(Note::Refused(vec![e]));
                continue;
            }
            Err(crate::sync::Refused::Errors(errors)) => {
                note(Note::Refused(errors));
                continue;
            }
            Ok(plan) if !plan.changes.is_empty() => {
                if !opts.sync {
                    note(Note::Stale(
                        plan.changes.iter().map(|c| c.path.clone()).collect(),
                    ));
                    continue;
                }
                if let Err(e) = crate::sync::apply(root, &plan) {
                    note(Note::Refused(vec![e.to_string()]));
                    continue;
                }
                note(Note::Synced(crate::sync::summary(&plan.changes)));
                // What sync wrote isn't a change to react to.
                base = snapshot(root);
            }
            Ok(_) => {}
        }
        note(Note::Building);
        let built = build(root, stop);
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let Some(program) = built else {
            note(Note::BuildFailed {
                still_running: running.is_some(),
            });
            continue;
        };
        // Edited again while building: build that instead of starting this.
        if snapshot(root) != base {
            continue;
        }
        if let Some(mut old) = running.take() {
            note(Note::Stopping(old.id()));
            if stop_child(&mut old, libc::SIGTERM) {
                note(Note::Killed(old.id()));
            }
        }
        match start(&program, root, &opts.args) {
            Ok(child) => {
                note(Note::Started(child.id()));
                running = Some(child);
            }
            Err(e) => note(Note::Exited(format!(
                "couldn't start {}: {e}",
                program.display()
            ))),
        }
    }
    if let Some(mut child) = running.take()
        && stop_child(&mut child, libc::SIGINT)
    {
        note(Note::Killed(child.id()));
    }
    note(Note::Done);
}

/// Set by SIGINT/SIGTERM.
static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

/// `bonsai dev [--sync] [-- <args for the tree>]`.
pub fn dev(args: &[String]) -> ! {
    let mut opts = Options::default();
    let mut rest = args.iter();
    while let Some(a) = rest.next() {
        match a.as_str() {
            "--sync" => opts.sync = true,
            "--" => {
                opts.args = rest.by_ref().cloned().collect();
            }
            _ => {
                eprintln!("usage: bonsai dev [--sync] [-- <args for the tree>]");
                std::process::exit(2);
            }
        }
    }
    crate::tree::require_tree_unchanged("dev");
    // SAFETY: the handler only stores to an atomic.
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
    }
    run(Path::new("."), &opts, &STOP, &mut |n| {
        eprintln!("bonsai dev: {}", n.text());
    });
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_sources_are_watched() {
        for p in [
            "bonsai.toml",
            "Cargo.toml",
            ".cargo/config.toml",
            "build.rs",
            "src/main.rs",
            "src/branches/sensor.rs",
        ] {
            assert!(watched(Path::new(p)), "{p}");
        }
        for p in [
            "Cargo.lock",
            "target/debug/tree",
            "logs/2026-10-01_10-00-00/events.log",
            "src/branches/.sensor.rs.swp",
            "src/.links.rs.bonsai-new",
            "src/notes.txt",
            ".bonsai-sync",
            "README.md",
        ] {
            assert!(!watched(Path::new(p)), "{p}");
        }
    }

    #[test]
    fn output_folders_are_never_walked() {
        let root = std::env::temp_dir().join(format!("bonsai-dev-snap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["src/branches", "target/debug", "logs/run"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("bonsai.toml"), "").unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        let before = snapshot(&root);
        std::fs::write(root.join("target/debug/tree"), "binary").unwrap();
        std::fs::write(root.join("logs/run/events.log"), "START").unwrap();
        std::fs::write(root.join("Cargo.lock"), "").unwrap();
        assert_eq!(changed(&before, &snapshot(&root)), Vec::<PathBuf>::new());
        std::fs::write(root.join("src/branches/sensor.rs"), "// new").unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() { }").unwrap();
        assert_eq!(
            changed(&before, &snapshot(&root)),
            [
                PathBuf::from("src/branches/sensor.rs"),
                PathBuf::from("src/main.rs")
            ]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_program_is_the_last_binary_cargo_built() {
        let json = "{\"reason\":\"compiler-artifact\",\"target\":{\"kind\":[\"lib\"]},\"executable\":null}\n\
                    {\"reason\":\"compiler-artifact\",\"target\":{\"kind\":[\"bin\"]},\"executable\":\"/t/target/debug/greenhouse\",\"fresh\":false}\n\
                    {\"reason\":\"build-finished\",\"success\":true}\n";
        assert_eq!(
            executable(json),
            Some(PathBuf::from("/t/target/debug/greenhouse"))
        );
        assert_eq!(
            executable("{\"reason\":\"build-finished\",\"success\":false}\n"),
            None
        );
    }

    /// A host tree in target/dev-loop/tree (its own target/ kept between
    /// runs, so only the first build is slow).
    fn render_tree() -> PathBuf {
        fn copy(from: &Path, to: &Path) {
            std::fs::create_dir_all(to).unwrap();
            for e in std::fs::read_dir(from).unwrap().flatten() {
                let path = e.path();
                let target = to.join(e.file_name());
                if path.is_dir() {
                    copy(&path, &target);
                } else {
                    let text = std::fs::read_to_string(&path).unwrap();
                    std::fs::write(target, text.replace("{{project-name}}", "dev_loop_tree"))
                        .unwrap();
                }
            }
        }
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        let root = repo.join("target/dev-loop/tree");
        for old in ["src", "logs", "bonsai.toml", "Cargo.toml", ".cargo"] {
            let _ = std::fs::remove_dir_all(root.join(old));
            let _ = std::fs::remove_file(root.join(old));
        }
        copy(&repo.join("templates/linux/host"), &root);
        root
    }

    fn alive(pid: u32) -> bool {
        // SAFETY: signal 0 only checks the pid exists.
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }

    /// The whole loop on a real tree: a first build and start, output
    /// folders and the tree's own logs ignored, a compile error that keeps
    /// the old program running, a fix that replaces it, a burst of edits
    /// taken as one, stale generated code not built, and a clean stop.
    #[test]
    #[ignore = "builds a real tree: cargo test -- --ignored dev_loop"]
    fn dev_loop_end_to_end() {
        use std::sync::{Arc, Mutex};
        let root = render_tree();
        let notes: Arc<Mutex<Vec<Note>>> = Arc::default();
        let stop: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
        let (r, n) = (root.clone(), notes.clone());
        let looping = std::thread::spawn(move || {
            run(&r, &Options::default(), stop, &mut |note| {
                eprintln!("bonsai dev: {}", note.text());
                n.lock().unwrap().push(note);
            })
        });
        let seen = |from: usize| notes.lock().unwrap()[from..].to_vec();
        let wait_for = |from: usize, within: Duration, what: &str, f: &dyn Fn(&Note) -> bool| {
            let until = Instant::now() + within;
            loop {
                if let Some(n) = seen(from).into_iter().find(|n| f(n)) {
                    return n;
                }
                assert!(Instant::now() < until, "no {what} in {:?}", seen(from));
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        let started = |n: &Note| matches!(n, Note::Started(_));
        let pid_of = |n: Note| match n {
            Note::Started(pid) => pid,
            other => panic!("{other:?}"),
        };
        let main_rs = root.join("src/main.rs");
        let main = std::fs::read_to_string(&main_rs).unwrap();

        // Built and started.
        let first = pid_of(wait_for(0, Duration::from_secs(600), "start", &started));
        assert!(alive(first));

        // The tree's run logs, a build's output, Cargo.lock: not changes.
        let mark = notes.lock().unwrap().len();
        std::fs::create_dir_all(root.join("target/extra")).unwrap();
        std::fs::write(root.join("target/extra/x"), "x").unwrap();
        std::fs::create_dir_all(root.join("logs/extra")).unwrap();
        std::fs::write(root.join("logs/extra/events.log"), "x").unwrap();
        std::fs::write(
            root.join("Cargo.lock"),
            std::fs::read(root.join("Cargo.lock")).unwrap_or_default(),
        )
        .unwrap();
        std::thread::sleep(SETTLE * 4);
        assert_eq!(seen(mark), [], "output files set off a rebuild");

        // A compile error: reported, and the running program is kept.
        let mark = notes.lock().unwrap().len();
        std::fs::write(
            &main_rs,
            format!("{main}\ncompile_error!(\"on purpose\");\n"),
        )
        .unwrap();
        let failed = wait_for(mark, Duration::from_secs(120), "build failure", &|n| {
            matches!(n, Note::BuildFailed { .. })
        });
        assert_eq!(
            failed,
            Note::BuildFailed {
                still_running: true
            }
        );
        assert!(alive(first), "a failed build stopped the running program");
        assert!(!seen(mark).iter().any(started));

        // Fixed: the old one is stopped, then the new one started.
        let mark = notes.lock().unwrap().len();
        std::fs::write(&main_rs, format!("{main}\n// fixed\n")).unwrap();
        let second = pid_of(wait_for(
            mark,
            Duration::from_secs(120),
            "restart",
            &started,
        ));
        let order = seen(mark);
        let stopping = order
            .iter()
            .position(|n| *n == Note::Stopping(first))
            .unwrap();
        let start = order
            .iter()
            .position(|n| *n == Note::Started(second))
            .unwrap();
        assert!(stopping < start, "{order:?}");
        assert!(
            second != first && alive(second) && !alive(first),
            "{order:?}"
        );

        // A burst of edits, each inside SETTLE of the last: one build.
        let mark = notes.lock().unwrap().len();
        for i in 0..5 {
            std::fs::write(&main_rs, format!("{main}\n// edit {i}\n")).unwrap();
            std::thread::sleep(SETTLE / 3);
        }
        let third = pid_of(wait_for(
            mark,
            Duration::from_secs(120),
            "restart",
            &started,
        ));
        std::thread::sleep(SETTLE * 4);
        let after = seen(mark);
        let builds = after.iter().filter(|n| **n == Note::Building).count();
        assert_eq!(builds, 1, "{after:?}");
        assert!(alive(third) && !alive(second));

        // Generated code out of date, and no --sync: not built.
        let mark = notes.lock().unwrap().len();
        let toml = std::fs::read_to_string(root.join("bonsai.toml")).unwrap();
        std::fs::write(
            root.join("bonsai.toml"),
            toml.replace("errors = false", "errors = true"),
        )
        .unwrap();
        let stale = wait_for(mark, Duration::from_secs(30), "stale", &|n| {
            matches!(n, Note::Stale(_))
        });
        assert_eq!(stale, Note::Stale(vec!["src/links.rs".to_string()]));
        std::thread::sleep(SETTLE * 2);
        assert!(!seen(mark).contains(&Note::Building));
        assert!(alive(third));
        std::fs::write(root.join("bonsai.toml"), toml).unwrap();

        // Stop: the program ends, then the loop.
        stop.store(true, Ordering::SeqCst);
        looping.join().unwrap();
        assert!(!alive(third), "the program outlived bonsai dev");
        assert_eq!(notes.lock().unwrap().last(), Some(&Note::Done));
        std::fs::write(&main_rs, main).unwrap();
    }
}

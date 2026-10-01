//! `bonsai dev` stops everything a program started, the real binary: with a
//! fake cargo (`CARGO`) building a fake program whose children escape its
//! process group (`setsid`), its parentage (a double fork) and its
//! environment (`env -i`). Each ignores SIGTERM, SIGINT and SIGHUP, so only
//! SIGKILL ends it. Each test runs with the program in a PID namespace (as
//! `bonsai dev` runs it where it can) and without (`BONSAI_DEV_NO_NAMESPACE`:
//! the subreaper and the guardian). The fake cargo leaves a daemon, as
//! sccache does, which is never the program's to stop.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BONSAI: &str = env!("CARGO_BIN_EXE_bonsai");

const FAKE_CARGO: &str = r#"#!/bin/sh
D="$(cd "$(dirname "$0")" && pwd)"
[ -e "$D/daemon.pid" ] || setsid -f sh -c 'echo $$ > "$1"; exec sleep 1000' sh "$D/daemon.pid"
printf '{"reason":"compiler-artifact","target":{"kind":["bin"]},"executable":"%s"}\n' "$D/app.sh"
"#;

/// Generation n leaves: a child in its group, one that left it with setsid
/// (still its child), an orphan (setsid -f: its parent exits at once), and
/// an orphan with an empty environment. Each writes its own pid as /proc
/// shows it: this computer's, even in the namespace (where `$$` is its own).
const FAKE_APP: &str = r#"#!/bin/sh
D="$(dirname "$0")"
n=$(( $(cat "$D/gen" 2>/dev/null || echo 0) + 1 ))
echo $n > "$D/gen"
read -r p _ < /proc/self/stat; echo $p > "$D/app$n.pid"
(trap '' TERM INT HUP; read -r p _ < /proc/self/stat; echo $p > "$D/app$n-grouped.pid"; exec sleep 1000) &
(exec setsid sh -c 'trap "" TERM INT HUP; read -r p _ < /proc/self/stat; echo $p > "$1"; exec sleep 1000' sh "$D/app$n-setsid.pid") &
setsid -f sh -c 'trap "" TERM INT HUP; read -r p _ < /proc/self/stat; echo $p > "$1"; exec sleep 1000' sh "$D/app$n-orphan.pid"
S="$(command -v sleep)"
env -i "$(command -v setsid)" -f /bin/sh -c 'trap "" TERM INT HUP; read -r p _ < /proc/self/stat; echo $p > "$1"; exec "$2" 1000' sh "$D/app$n-bare.pid" "$S"
trap 'exit 0' TERM INT
while :; do sleep 0.05; done
"#;

struct Tree {
    root: PathBuf,
    fake: PathBuf,
}

impl Tree {
    fn new(name: &str) -> Tree {
        fn copy(from: &Path, to: &Path) {
            std::fs::create_dir_all(to).unwrap();
            for e in std::fs::read_dir(from).unwrap().flatten() {
                let (path, target) = (e.path(), to.join(e.file_name()));
                if path.is_dir() {
                    copy(&path, &target);
                } else {
                    let text = std::fs::read_to_string(&path).unwrap();
                    std::fs::write(target, text.replace("{{project-name}}", "dev_procs")).unwrap();
                }
            }
        }
        let root =
            std::env::temp_dir().join(format!("bonsai-dev-procs-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        copy(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("templates/linux/host"),
            &root,
        );
        let fake = root.join("fake");
        std::fs::create_dir_all(&fake).unwrap();
        for (file, text) in [("cargo.sh", FAKE_CARGO), ("app.sh", FAKE_APP)] {
            std::fs::write(fake.join(file), text).unwrap();
            std::fs::set_permissions(fake.join(file), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        Tree { root, fake }
    }

    /// `bonsai dev` in it, its output to a file (so nothing it leaves
    /// behind holds a pipe of ours).
    fn dev(&self, namespace: bool) -> Child {
        let log = std::fs::File::create(self.root.join("dev.log")).unwrap();
        Command::new(BONSAI)
            .arg("dev")
            .env("BONSAI_DEV_NO_NAMESPACE", if namespace { "" } else { "1" })
            .current_dir(&self.root)
            .env("CARGO", self.fake.join("cargo.sh"))
            .env("BONSAI_TOP", "off")
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap()
    }

    fn pid(&self, file: &str) -> u32 {
        let path = self.fake.join(file);
        wait_until(file, || {
            std::fs::read_to_string(&path).is_ok_and(|t| t.trim().parse::<u32>().is_ok())
        });
        std::fs::read_to_string(&path)
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    /// Generation n's processes: the program and the four it left.
    fn generation(&self, n: u32) -> [u32; 5] {
        ["", "-grouped", "-setsid", "-orphan", "-bare"]
            .map(|kind| self.pid(&format!("app{n}{kind}.pid")))
    }

    fn edit(&self) {
        let main = self.root.join("src/main.rs");
        let text = std::fs::read_to_string(&main).unwrap();
        std::fs::write(&main, format!("{text}// edit\n")).unwrap();
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.root.join("dev.log")).unwrap_or_default()
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// `pid`'s state letter and parent, while it exists.
fn stat(pid: u32) -> Option<(String, u32)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    Some((f.first()?.to_string(), f.get(1)?.parse().ok()?))
}

fn running(pid: u32) -> bool {
    stat(pid).is_some_and(|(state, _)| state != "Z")
}

fn wait_until(what: &str, done: impl Fn() -> bool) {
    let until = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < until, "no {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The guardian `bonsai dev` (pid `dev`) started, if it's running.
fn guardian_of(dev: u32) -> Option<u32> {
    let want = format!("__dev-guardian\0{dev}\0");
    std::fs::read_dir("/proc").ok()?.flatten().find_map(|e| {
        let pid: u32 = e.file_name().to_str()?.parse().ok()?;
        let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        (String::from_utf8_lossy(&cmdline).contains(&want) && running(pid)).then_some(pid)
    })
}

/// SIGKILL whatever of `pids` is left, so a failed test leaves nothing.
fn sweep(pids: &[u32]) {
    for &pid in pids {
        if running(pid) {
            // SAFETY: test cleanup of processes this test started.
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        }
    }
}

/// Wait for `bonsai dev` to exit, at most 20 s.
fn exited(dev: &mut Child, tree: &Tree) -> std::process::ExitStatus {
    let until = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = dev.try_wait().unwrap() {
            return status;
        }
        assert!(
            Instant::now() < until,
            "bonsai dev didn't stop\n{}",
            tree.log()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Wait up to 20 s for every one of `pids` to end; those still running.
fn left_after(pids: &[u32]) -> Vec<u32> {
    let until = Instant::now() + Duration::from_secs(20);
    while pids.iter().any(|&p| running(p)) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    let left: Vec<u32> = pids.iter().copied().filter(|&p| running(p)).collect();
    sweep(pids);
    left
}

/// The processes running `bonsai __dev-run` under `bonsai dev` (pid `dev`).
fn wrappers_of(dev: u32) -> Vec<u32> {
    let want = format!("__dev-run\0{dev}\0");
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/cmdline"))
                .is_ok_and(|c| String::from_utf8_lossy(&c).contains(&want))
        })
        .collect()
}

/// Whether this computer lets `bonsai dev` make a PID namespace; if not,
/// a namespace test says it's skipped.
fn namespaces_here(test: &str) -> bool {
    let ok = Command::new(BONSAI)
        .args(["__dev-run", "--probe"])
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("{test}: skipped: no PID namespace can be made here");
    }
    ok
}

/// Restart and Ctrl-C stop everything generation 1 and 2 left, nothing
/// else (a stranger, the build's daemon), and the guardian goes.
fn restart_and_ctrl_c(namespace: bool) {
    let tree = Tree::new(if namespace { "escape-ns" } else { "escape" });
    let mut stranger = Command::new("sleep").arg("1000").spawn().unwrap();
    let mut dev = tree.dev(namespace);
    let first = tree.generation(1);
    let daemon = tree.pid("daemon.pid");
    let guardian = guardian_of(dev.id()).expect("no guardian");
    assert_eq!(
        wrappers_of(dev.id()).is_empty(),
        !namespace,
        "{}",
        tree.log()
    );
    if !namespace {
        // The orphans' parent exited: `bonsai dev` adopted them.
        wait_until("the orphans' adoption", || {
            [first[3], first[4]]
                .iter()
                .all(|&p| stat(p).is_some_and(|(_, parent)| parent == dev.id()))
        });
    }
    tree.edit();
    let second = tree.generation(2);
    let gone: Vec<u32> = first.iter().copied().filter(|&p| running(p)).collect();
    sweep(&first);
    assert!(
        gone.is_empty(),
        "left running after the restart: {gone:?}\n{}",
        tree.log()
    );
    // Ctrl-C (SIGINT, as a terminal sends it).
    // SAFETY: signals the bonsai dev this test started.
    unsafe { libc::kill(dev.id() as i32, libc::SIGINT) };
    let status = exited(&mut dev, &tree);
    let gone: Vec<u32> = second.iter().copied().filter(|&p| running(p)).collect();
    sweep(&second);
    let daemon_ran = running(daemon);
    sweep(&[daemon]);
    assert!(status.success(), "{status}\n{}", tree.log());
    assert!(
        gone.is_empty(),
        "left running after Ctrl-C: {gone:?}\n{}",
        tree.log()
    );
    assert!(daemon_ran, "the build's daemon was stopped");
    assert!(
        running(stranger.id()),
        "a process bonsai dev didn't start was signalled"
    );
    stranger.kill().unwrap();
    stranger.wait().unwrap();
    // Told it's done, the guardian goes too.
    wait_until("the guardian's exit", || !running(guardian));
}

#[test]
fn what_left_the_group_goes_on_restart_and_on_ctrl_c() {
    if namespaces_here("restart_and_ctrl_c") {
        restart_and_ctrl_c(true);
    }
}

#[test]
fn what_left_the_group_goes_on_restart_and_on_ctrl_c_without_a_namespace() {
    restart_and_ctrl_c(false);
}

/// SIGKILL `bonsai dev` (and, with `everything`, the guardian and the
/// namespace's wrapper too): nothing generation 1 started is left.
fn killed(namespace: bool, everything: bool) {
    let tree = Tree::new(&format!("killed-{namespace}-{everything}"));
    let mut stranger = Command::new("sleep").arg("1000").spawn().unwrap();
    let mut dev = tree.dev(namespace);
    let first = tree.generation(1);
    let daemon = tree.pid("daemon.pid");
    let guardian = guardian_of(dev.id()).expect("no guardian");
    if !namespace {
        // Without a namespace, an orphan with no environment is found only
        // by the guardian's last look (every 100 ms) before dev dies.
        std::thread::sleep(Duration::from_millis(500));
    }
    let wrappers = wrappers_of(dev.id());
    dev.kill().unwrap(); // SIGKILL: it can't clean up
    if everything {
        for &pid in wrappers.iter().chain([&guardian]) {
            // SAFETY: SIGKILL to processes this test's bonsai dev started.
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        }
    }
    dev.wait().unwrap();
    let left = left_after(&first);
    let daemon_ran = running(daemon);
    sweep(&[daemon]);
    assert!(
        left.is_empty(),
        "left running after bonsai dev was killed: {left:?}\n{}",
        tree.log()
    );
    assert!(daemon_ran, "the build's daemon was stopped");
    wait_until("the guardian's exit", || !running(guardian));
    assert!(
        running(stranger.id()),
        "a process bonsai dev didn't start was signalled"
    );
    stranger.kill().unwrap();
    stranger.wait().unwrap();
}

#[test]
fn killing_bonsai_dev_outright_leaves_nothing_running() {
    if namespaces_here("killed") {
        killed(true, false);
    }
}

#[test]
fn killing_bonsai_dev_and_its_helpers_outright_leaves_nothing_running() {
    if namespaces_here("killed with its helpers") {
        killed(true, true);
    }
}

#[test]
fn killing_bonsai_dev_outright_leaves_nothing_running_without_a_namespace() {
    killed(false, false);
}

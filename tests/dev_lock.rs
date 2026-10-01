//! `bonsai dev` and the tree's lock (`src/lock.rs`), the real binary: while
//! another command holds the tree, a build that's owed waits and then starts
//! by itself, with no save needed; the running program stays until its
//! replacement is built; edits made meanwhile are what's built; Ctrl-C still
//! stops it promptly; and a lock that can't be taken at all is said once,
//! not retried in a tight loop. The fake cargo keeps a copy of `src/main.rs`
//! for each build it's asked for (`built-<n>`).

use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BONSAI: &str = env!("CARGO_BIN_EXE_bonsai");

const FAKE_CARGO: &str = r#"#!/bin/sh
D="$(cd "$(dirname "$0")" && pwd)"
n=$(( $(ls "$D" | grep -c '^built-') + 1 ))
cp src/main.rs "$D/built-$n"
printf '{"reason":"compiler-artifact","target":{"kind":["bin"]},"executable":"%s"}\n' "$D/app.sh"
"#;

/// Generation n writes its pid (as /proc shows it, even in a namespace) to
/// `app<n>.pid`, and exits on SIGTERM/SIGINT.
const FAKE_APP: &str = r#"#!/bin/sh
D="$(dirname "$0")"
n=$(( $(cat "$D/gen" 2>/dev/null || echo 0) + 1 ))
echo $n > "$D/gen"
read -r p _ < /proc/self/stat; echo $p > "$D/app$n.pid"
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
                    std::fs::write(target, text.replace("{{project-name}}", "dev_lock")).unwrap();
                }
            }
        }
        let root =
            std::env::temp_dir().join(format!("bonsai-dev-lock-{}-{name}", std::process::id()));
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

    /// `bonsai dev`, its output to a file, giving up on the lock after
    /// 0.3 s (`BONSAI_LOCK_WAIT`, which it now only reports at).
    fn dev(&self) -> Child {
        let log = File::create(self.root.join("dev.log")).unwrap();
        Command::new(BONSAI)
            .arg("dev")
            .current_dir(&self.root)
            .env("CARGO", self.fake.join("cargo.sh"))
            .env("BONSAI_TOP", "off")
            .env("BONSAI_LOCK_WAIT", "0.3")
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap()
    }

    /// The tree's lock, held as another bonsai command would.
    fn hold(&self) -> File {
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.root.join(".bonsai.lock"))
            .unwrap();
        // SAFETY: flock on a descriptor we own.
        assert_eq!(unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) }, 0);
        file
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.root.join("dev.log")).unwrap_or_default()
    }

    fn builds(&self) -> usize {
        std::fs::read_dir(&self.fake)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("built-"))
            .count()
    }

    fn built(&self, n: usize) -> String {
        std::fs::read_to_string(self.fake.join(format!("built-{n}"))).unwrap_or_default()
    }

    /// The pid of generation `n` of the program, once it has started.
    fn app(&self, n: u32) -> u32 {
        let path = self.fake.join(format!("app{n}.pid"));
        wait_until(&format!("program {n}\n{}", self.log()), || {
            std::fs::read_to_string(&path).is_ok_and(|t| t.trim().parse::<u32>().is_ok())
        });
        std::fs::read_to_string(&path)
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    fn edit(&self, text: &str) {
        let main = self.root.join("src/main.rs");
        let old = std::fs::read_to_string(&main).unwrap();
        std::fs::write(&main, format!("{old}// {text}\n")).unwrap();
    }

    fn main_rs(&self) -> String {
        std::fs::read_to_string(self.root.join("src/main.rs")).unwrap()
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn wait_until(what: &str, done: impl Fn() -> bool) {
    let until = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < until, "no {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn running(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| {
        s.rfind(')')
            .is_some_and(|i| !s[i + 1..].trim_start().starts_with('Z'))
    })
}

/// SIGINT `dev` (Ctrl-C) and wait for it, at most `within`.
fn ctrl_c(dev: &mut Child, tree: &Tree, within: Duration) -> Duration {
    let started = Instant::now();
    // SAFETY: signals the bonsai dev this test started.
    unsafe { libc::kill(dev.id() as i32, libc::SIGINT) };
    loop {
        if let Some(status) = dev.try_wait().unwrap() {
            assert!(status.success(), "{status}\n{}", tree.log());
            return started.elapsed();
        }
        if started.elapsed() > within {
            let _ = dev.kill();
            panic!("bonsai dev didn't stop within {within:?}\n{}", tree.log());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Its CPU time so far (user + system), from /proc.
fn cpu(pid: u32) -> Duration {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let f: Vec<u64> = stat[stat.rfind(')').unwrap() + 1..]
        .split_whitespace()
        .skip(11)
        .take(2)
        .map(|v| v.parse().unwrap())
        .collect();
    // SAFETY: sysconf has no preconditions.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64;
    Duration::from_millis((f[0] + f[1]) * 1000 / hz)
}

#[test]
fn the_first_build_starts_once_the_lock_is_let_go_with_no_save() {
    let tree = Tree::new("first");
    let held = tree.hold();
    let mut dev = tree.dev();
    // Well past BONSAI_LOCK_WAIT: still waiting, nothing built.
    std::thread::sleep(Duration::from_millis(1500));
    let log = tree.log();
    assert_eq!(tree.builds(), 0, "{log}");
    assert_eq!(log.matches("waiting for").count(), 1, "{log}");
    assert_eq!(log.matches("still holds the tree").count(), 1, "{log}");
    let before = tree.main_rs();
    drop(held);
    tree.app(1);
    assert_eq!(tree.builds(), 1, "{}", tree.log());
    assert_eq!(tree.main_rs(), before, "the test changed a file");
    ctrl_c(&mut dev, &tree, Duration::from_secs(10));
}

#[test]
fn the_program_stays_until_the_latest_edit_is_built() {
    let tree = Tree::new("pending");
    let mut dev = tree.dev();
    let first = tree.app(1);
    assert_eq!(tree.builds(), 1);
    let held = tree.hold();
    tree.edit("one");
    std::thread::sleep(Duration::from_millis(1500));
    // Owed a build, but the tree is held: the program carries on.
    assert!(running(first), "{}", tree.log());
    assert_eq!(tree.builds(), 1, "{}", tree.log());
    assert!(tree.log().contains("waiting for"), "{}", tree.log());
    // More edits while it waits: the build is of the last one.
    tree.edit("two");
    std::thread::sleep(Duration::from_millis(700));
    tree.edit("three");
    std::thread::sleep(Duration::from_millis(700));
    assert!(running(first), "{}", tree.log());
    let latest = tree.main_rs();
    drop(held);
    let second = tree.app(2);
    assert!(!running(first), "the old program is still running");
    assert!(running(second));
    assert_eq!(tree.builds(), 2, "{}", tree.log());
    assert_eq!(
        tree.built(2),
        latest,
        "built something other than the latest"
    );
    // Built once: nothing more, with nothing changed.
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(tree.builds(), 2, "{}", tree.log());
    ctrl_c(&mut dev, &tree, Duration::from_secs(10));
    assert!(!running(second), "Ctrl-C left the program running");
}

#[test]
fn ctrl_c_while_waiting_for_the_lock_stops_promptly() {
    // Before anything was built.
    let tree = Tree::new("stop-first");
    let held = tree.hold();
    let mut dev = tree.dev();
    wait_until("wait for the lock", || tree.log().contains("waiting for"));
    std::thread::sleep(Duration::from_millis(1200)); // backing off by now
    let took = ctrl_c(&mut dev, &tree, Duration::from_secs(5));
    assert!(took < Duration::from_secs(2), "took {took:?}");
    assert_eq!(tree.builds(), 0);
    drop(held);
    // With a program running and a build owed.
    let tree = Tree::new("stop-running");
    let mut dev = tree.dev();
    let first = tree.app(1);
    let _held = tree.hold();
    tree.edit("owed");
    wait_until("wait for the lock", || tree.log().contains("waiting for"));
    std::thread::sleep(Duration::from_millis(1200));
    let took = ctrl_c(&mut dev, &tree, Duration::from_secs(10));
    assert!(took < Duration::from_secs(3), "took {took:?}");
    assert!(!running(first), "Ctrl-C left the program running");
    assert_eq!(tree.builds(), 1);
}

#[test]
fn a_lock_that_cant_be_taken_is_said_once_and_not_retried() {
    let tree = Tree::new("broken");
    // A lock file that can't be opened: a symlink to itself.
    std::os::unix::fs::symlink(".bonsai.lock", tree.root.join(".bonsai.lock")).unwrap();
    let mut dev = tree.dev();
    wait_until("report", || tree.log().contains("not building"));
    let spent = cpu(dev.id());
    std::thread::sleep(Duration::from_secs(2));
    let log = tree.log();
    assert_eq!(log.matches("not building").count(), 1, "{log}");
    assert!(log.contains("can't open"), "{log}");
    assert!(!log.contains("waiting for"), "taken for contention: {log}");
    assert_eq!(tree.builds(), 0);
    // Idle, not spinning: well under a tenth of the time on the CPU.
    let busy = cpu(dev.id()) - spent;
    assert!(busy < Duration::from_millis(200), "{busy:?} of CPU in 2s");
    // Fixed, and saved: it builds.
    std::fs::remove_file(tree.root.join(".bonsai.lock")).unwrap();
    tree.edit("fixed");
    tree.app(1);
    ctrl_c(&mut dev, &tree, Duration::from_secs(10));
}

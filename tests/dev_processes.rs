//! `bonsai dev` stops everything a program started, the real binary: with a
//! fake cargo (`CARGO`) building a fake program whose children escape its
//! process group (`setsid`) and its parentage (a double fork, so `bonsai
//! dev` adopts the orphan as a subreaper). Each ignores SIGTERM, SIGINT and
//! SIGHUP, so only SIGKILL ends it.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BONSAI: &str = env!("CARGO_BIN_EXE_bonsai");

const FAKE_CARGO: &str = r#"#!/bin/sh
D="$(cd "$(dirname "$0")" && pwd)"
printf '{"reason":"compiler-artifact","target":{"kind":["bin"]},"executable":"%s"}\n' "$D/app.sh"
"#;

/// Generation n leaves: a child in its group, one that left it with setsid
/// (still its child), and an orphan (setsid -f: its parent exits at once).
const FAKE_APP: &str = r#"#!/bin/sh
D="$(dirname "$0")"
n=$(( $(cat "$D/gen" 2>/dev/null || echo 0) + 1 ))
echo $n > "$D/gen"
echo $$ > "$D/app$n.pid"
(trap '' TERM INT HUP; exec sleep 1000) &
echo $! > "$D/app$n-grouped.pid"
(exec setsid sh -c 'trap "" TERM INT HUP; exec sleep 1000') &
echo $! > "$D/app$n-setsid.pid"
setsid -f sh -c 'echo $$ > "$1"; trap "" TERM INT HUP; exec sleep 1000' sh "$D/app$n-orphan.pid"
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
    fn dev(&self) -> Child {
        let log = std::fs::File::create(self.root.join("dev.log")).unwrap();
        Command::new(BONSAI)
            .arg("dev")
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

    /// Generation n's processes: the program and the three it left.
    fn generation(&self, n: u32) -> [u32; 4] {
        ["", "-grouped", "-setsid", "-orphan"].map(|kind| self.pid(&format!("app{n}{kind}.pid")))
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

/// SIGKILL whatever of `pids` is left, so a failed test leaves nothing.
fn sweep(pids: &[u32]) {
    for &pid in pids {
        if running(pid) {
            // SAFETY: test cleanup of processes this test started.
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        }
    }
}

#[test]
fn what_left_the_group_goes_on_restart_and_on_ctrl_c() {
    let tree = Tree::new("escape");
    let mut stranger = Command::new("sleep").arg("1000").spawn().unwrap();
    let mut dev = tree.dev();
    let first = tree.generation(1);
    // The orphan's parent exited: `bonsai dev` adopted it.
    wait_until("the orphan's adoption", || {
        stat(first[3]).is_some_and(|(_, p)| p == dev.id())
    });
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
    let until = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = dev.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < until,
            "bonsai dev didn't stop\n{}",
            tree.log()
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let gone: Vec<u32> = second.iter().copied().filter(|&p| running(p)).collect();
    sweep(&second);
    assert!(status.success(), "{status}\n{}", tree.log());
    assert!(
        gone.is_empty(),
        "left running after Ctrl-C: {gone:?}\n{}",
        tree.log()
    );
    assert!(
        running(stranger.id()),
        "a process bonsai dev didn't start was signalled"
    );
    stranger.kill().unwrap();
    stranger.wait().unwrap();
}

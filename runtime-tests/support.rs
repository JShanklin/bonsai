//! Helpers for the runtime tests: child processes, a scratch folder, and a
//! small tree to run.
#![allow(dead_code)] // not every test module uses every helper

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::bonsai::{Event, Tree};

/// Set in a child process started by `child`.
pub const CHILD: &str = "BONSAI_RUNTIME_TEST_CHILD";

/// Whether this process is a child started for one scenario.
pub fn is_child() -> bool {
    std::env::var_os(CHILD).is_some()
}

/// What a child process did.
#[derive(Debug)]
pub struct Ran {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

/// A running child, for tests that signal it. Its output is read as it
/// comes (a child that logs a lot would otherwise block on a full pipe).
pub struct Running {
    child: std::process::Child,
    started: Instant,
    stdout: std::thread::JoinHandle<String>,
    stderr: std::thread::JoinHandle<String>,
}

fn drain(from: impl std::io::Read + Send + 'static) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut from = from;
        let mut all = Vec::new();
        let _ = from.read_to_end(&mut all);
        String::from_utf8_lossy(&all).into_owned()
    })
}

/// Start `scenario` (a test path, `#[ignore]`d so it only runs when asked) in
/// a fresh copy of this test binary.
pub fn spawn(scenario: &str, env: &[(&str, &str)]) -> Running {
    let exe = std::env::current_exe().expect("test binary");
    let mut cmd = Command::new(exe);
    cmd.args([
        scenario,
        "--exact",
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ])
    .env(CHILD, "1")
    .env_remove("RUST_BACKTRACE")
    .env("BONSAI_TOP", "off")
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("start child");
    let stdout = drain(child.stdout.take().expect("stdout"));
    let stderr = drain(child.stderr.take().expect("stderr"));
    Running {
        child,
        started: Instant::now(),
        stdout,
        stderr,
    }
}

impl Running {
    pub fn pid(&self) -> i32 {
        self.child.id() as i32
    }

    pub fn signal(&self, signal: i32) {
        // SAFETY: kill only sends a signal to our own child.
        unsafe { libc::kill(self.pid(), signal) };
    }

    /// Wait up to `timeout`, then kill it.
    pub fn wait(mut self, timeout: Duration) -> Ran {
        let mut timed_out = false;
        loop {
            match self.child.try_wait().expect("wait") {
                Some(_) => break,
                None if self.started.elapsed() > timeout => {
                    timed_out = true;
                    let _ = self.child.kill();
                    break;
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        let status = self.child.wait().expect("wait");
        Ran {
            code: status.code(),
            stdout: self.stdout.join().unwrap_or_default(),
            stderr: self.stderr.join().unwrap_or_default(),
            timed_out,
        }
    }
}

/// Run `scenario` in a child and wait for it.
pub fn child(scenario: &str, env: &[(&str, &str)], timeout: Duration) -> Ran {
    spawn(scenario, env).wait(timeout)
}

/// A fresh, empty folder for one test.
pub fn scratch(name: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "bonsai-rt-{}-{name}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Open file descriptors of this process.
pub fn open_fds() -> usize {
    std::fs::read_dir("/proc/self/fd").map_or(0, |d| d.count())
}

/// A free TCP port on 127.0.0.1.
pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .expect("free port")
}

/// A tree with one ticking branch-like handler: `handle` runs `on_tick` for
/// each tick, which is how scenarios log and record from "inside" a tree.
pub struct TickTree {
    pub hz: f64,
    pub on_tick: Box<dyn FnMut(u64)>,
    pub ticks: u64,
}

impl Tree for TickTree {
    type EdgeIn = ();

    fn rates(&self) -> Vec<(usize, f64)> {
        vec![(0, self.hz)]
    }

    fn start_edges(&mut self, _events: &mpsc::Sender<Event<()>>) {}

    fn handle(&mut self, event: Event<()>) {
        if let Event::Tick(_) = event {
            self.ticks += 1;
            (self.on_tick)(self.ticks);
        }
    }
}

/// Run `tree` until SIGTERM/Ctrl-C, on a runtime like a tree's own.
pub fn run_tree<T: Tree>(tree: T) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(crate::bonsai::run(tree));
}

//! Run logs, in child processes (the recorder, the logger and the signal
//! handlers are process-wide).

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::support::{TickTree, child, is_child, run_tree, scratch, spawn};
use crate::bonsai::record;

/// The recorder's settings for a scenario: `RT_DIR` from the parent, and
/// which kinds `RT_KINDS` lists.
fn configure() {
    let dir: &'static str = Box::leak(std::env::var("RT_DIR").unwrap().into_boxed_str());
    let kinds = std::env::var("RT_KINDS").unwrap_or_else(|_| "events".into());
    record::configure(record::Config {
        dir,
        events: kinds.contains("events"),
        panics: kinds.contains("panics"),
        errors: kinds.contains("errors"),
        edges: kinds.contains("edges"),
        ..record::Config::DEFAULT
    });
}

fn stop_self() {
    // SAFETY: signals this process, as systemd or Ctrl-C would.
    unsafe { libc::kill(libc::getpid(), libc::SIGTERM) };
}

/// A tree ticking 50 times a second: tick `n` calls `on`, and it stops
/// itself at tick `stop` (never, when 0).
fn ticking(stop: u64, mut on: impl FnMut(u64) + 'static) -> TickTree {
    TickTree {
        hz: 50.0,
        ticks: 0,
        on_tick: Box::new(move |n| {
            on(n);
            println!("TICK {n}");
            if n == stop {
                stop_self();
            }
        }),
    }
}

/// The run folders in `dir`, oldest first.
fn runs(dir: &Path) -> Vec<PathBuf> {
    let mut runs: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    runs.sort_by_key(|p| p.metadata().and_then(|m| m.modified()).ok());
    runs
}

fn read(path: impl AsRef<Path>) -> String {
    std::fs::read_to_string(path.as_ref())
        .unwrap_or_else(|e| panic!("{}: {e}", path.as_ref().display()))
}

/// How many ticks a child printed (`TICK n`, which may share a line with
/// libtest's own output).
fn ticks(out: &str) -> usize {
    out.matches("TICK ").count()
}

// -- scenarios (run only as children) -----------------------------------------

#[test]
#[ignore = "a child scenario"]
fn scenario_ticks_until_stopped() {
    if !is_child() {
        return;
    }
    configure();
    let stop = std::env::var("RT_STOP").map_or(0, |s| s.parse().unwrap());
    run_tree(ticking(stop, |n| {
        if n == 2 {
            record!("launch {n}");
        }
        if n == 3 {
            warn!("boom at tick {n}");
        }
    }));
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let hwm = status
        .lines()
        .find(|l| l.starts_with("VmHWM:"))
        .unwrap_or("");
    println!("{hwm}");
}

// -- tests ---------------------------------------------------------------------

const SCENARIO: &str = "runtime_tests::record::scenario_ticks_until_stopped";

#[test]
fn sigterm_ends_every_file_with_end_after_its_records() {
    let dir = scratch("sigterm");
    let ran = child(
        SCENARIO,
        &[
            ("RT_DIR", dir.to_str().unwrap()),
            ("RT_KINDS", "events,errors"),
            ("RT_STOP", "10"),
        ],
        Duration::from_secs(10),
    );
    assert!(!ran.timed_out && ran.code == Some(0), "{ran:?}");
    let run = runs(&dir).pop().expect("a run folder");
    for file in ["events.log", "errors.log"] {
        let text = read(run.join(file));
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].contains(" START "), "{file}: {text}");
        assert!(
            lines.last().unwrap().contains(" END SIGTERM"),
            "{file}: {text}"
        );
        assert!(text.ends_with('\n'), "{file}: {text:?}");
    }
    assert!(read(run.join("events.log")).contains("launch 2"));
}

#[test]
fn errors_are_recorded_even_when_the_console_shows_none() {
    let dir = scratch("console-off");
    let ran = child(
        SCENARIO,
        &[
            ("RT_DIR", dir.to_str().unwrap()),
            ("RT_KINDS", "errors"),
            ("RT_STOP", "6"),
            ("BONSAI_LOG", "off"),
        ],
        Duration::from_secs(10),
    );
    assert!(ran.code == Some(0), "{ran:?}");
    assert!(
        !ran.stderr.contains("boom"),
        "BONSAI_LOG=off still printed: {}",
        ran.stderr
    );
    let run = runs(&dir).pop().expect("a run folder");
    let errors = read(run.join("errors.log"));
    assert!(errors.contains("WARN boom at tick 3"), "{errors}");
}

#[test]
fn a_run_that_never_ended_is_called_unclean_not_given_a_cause() {
    let dir = scratch("unclean");
    let env = [("RT_DIR", dir.to_str().unwrap())];
    let running = spawn(SCENARIO, &env);
    std::thread::sleep(Duration::from_millis(800));
    running.signal(libc::SIGKILL);
    let _ = running.wait(Duration::from_secs(5));
    std::thread::sleep(Duration::from_millis(1100)); // a new folder name
    let ran = child(
        SCENARIO,
        &[env[0], ("RT_STOP", "3")],
        Duration::from_secs(10),
    );
    assert!(ran.code == Some(0), "{ran:?}");
    let last = runs(&dir).pop().unwrap();
    let text = read(last.join("events.log"));
    assert!(
        text.contains("previous run"),
        "the killed run went unnoticed: {text}"
    );
    assert!(text.contains("did not shut down cleanly"), "{text}");
    assert!(
        !text.contains("lost power"),
        "a cause it can't know: {text}"
    );
}

#[test]
fn a_truncated_end_line_is_not_a_clean_end() {
    let dir = scratch("truncated");
    let old = dir.join("2000-01-01_00-00-00");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(
        old.join("events.log"),
        "2000-01-01 00:00:00.000 START t 0.1.0 on h (pid 1, UTC)\n2000-01-01 00:00:05.000 END SIGTE",
    )
    .unwrap();
    let ran = child(
        SCENARIO,
        &[("RT_DIR", dir.to_str().unwrap()), ("RT_STOP", "3")],
        Duration::from_secs(10),
    );
    assert!(ran.code == Some(0), "{ran:?}");
    let text = read(runs(&dir).pop().unwrap().join("events.log"));
    assert!(text.contains("previous run 2000-01-01_00-00-00"), "{text}");
}

#[test]
fn a_huge_previous_log_is_checked_without_reading_it_all() {
    let dir = scratch("huge");
    let old = dir.join("2000-01-01_00-00-00");
    std::fs::create_dir_all(&old).unwrap();
    let mut big = String::from("2000-01-01 00:00:00.000 START t 0.1.0 on h (pid 1, UTC)\n");
    let line = "2000-01-01 00:00:01.000 sensor: ".to_string() + &"x".repeat(200) + "\n";
    while big.len() < 96 << 20 {
        big.push_str(&line);
    }
    big.push_str("2000-01-01 00:10:00.000 END SIGTERM, after 10m00s\n");
    std::fs::write(old.join("events.log"), big).unwrap();
    let ran = child(
        SCENARIO,
        &[("RT_DIR", dir.to_str().unwrap()), ("RT_STOP", "3")],
        Duration::from_secs(10),
    );
    assert!(ran.code == Some(0), "{ran:?}");
    let text = read(runs(&dir).pop().unwrap().join("events.log"));
    assert!(!text.contains("previous run"), "{text}");
    // VmHWM: the most memory the child ever held, in kB.
    let hwm: u64 = ran
        .stdout
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        .expect("VmHWM");
    assert!(hwm < 48 * 1024, "the check held {hwm} kB");
}

#[test]
fn a_run_still_going_is_not_called_unclean_by_the_next() {
    let dir = scratch("concurrent");
    let env = [("RT_DIR", dir.to_str().unwrap())];
    let first = spawn(SCENARIO, &env);
    std::thread::sleep(Duration::from_millis(1200));
    let second = child(
        SCENARIO,
        &[env[0], ("RT_STOP", "3")],
        Duration::from_secs(10),
    );
    first.signal(libc::SIGTERM);
    let first = first.wait(Duration::from_secs(10));
    assert!(
        first.code == Some(0) && second.code == Some(0),
        "{first:?}\n{second:?}"
    );
    let runs = runs(&dir);
    assert_eq!(runs.len(), 2, "{runs:?}");
    for run in &runs {
        let text = read(run.join("events.log"));
        assert!(!text.contains("previous run"), "{}: {text}", run.display());
        assert!(text.contains(" END SIGTERM"), "{}: {text}", run.display());
    }
}

#[test]
fn unwritable_storage_never_stops_the_tree() {
    let dir = scratch("unwritable");
    let file = dir.join("not-a-folder");
    std::fs::write(&file, "").unwrap();
    let ran = child(
        SCENARIO,
        &[
            ("RT_DIR", file.join("logs").to_str().unwrap()),
            ("RT_STOP", "10"),
        ],
        Duration::from_secs(10),
    );
    assert!(ran.code == Some(0), "{ran:?}");
    assert_eq!(ticks(&ran.stdout), 10, "{ran:?}");
    assert!(ran.stderr.contains("run logs"), "{}", ran.stderr);
}

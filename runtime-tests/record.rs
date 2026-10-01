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
        keep_runs: env_or("RT_KEEP_RUNS", record::KEEP_RUNS),
        keep_days: env_or("RT_KEEP_DAYS", 0),
        max_file_kb: env_or("RT_MAX_FILE_KB", record::MAX_FILE_KB),
    });
}

fn env_or(key: &str, default: u32) -> u32 {
    std::env::var(key).map_or(default, |v| v.parse().unwrap())
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
    let var = |k: &str| std::env::var(k).map_or(0, |s| s.parse::<u64>().unwrap());
    let stop = var("RT_STOP");
    // A slow or failing disk, and a burst of records every tick.
    record::fault::SLOW_MS.store(var("RT_SLOW_MS"), std::sync::atomic::Ordering::Relaxed);
    record::fault::FAIL.store(var("RT_FAIL") == 1, std::sync::atomic::Ordering::Relaxed);
    // Kinds (a bit each, in `Kind` order) whose files can't be opened, and
    // whose writes fail from tick RT_WRITE_FAIL_AT on.
    record::fault::OPEN_FAIL.store(
        var("RT_OPEN_FAIL") as u8,
        std::sync::atomic::Ordering::Relaxed,
    );
    let write_fail = var("RT_WRITE_FAIL") as u8;
    let write_fail_when = std::env::var("RT_WRITE_FAIL_WHEN").unwrap_or_default();
    let burst = var("RT_BURST");
    run_tree(ticking(stop, move |n| {
        if write_fail != 0 && Path::new(&write_fail_when).exists() {
            record::fault::WRITE_FAIL.store(write_fail, std::sync::atomic::Ordering::Relaxed);
        }
        for i in 0..burst {
            record!("burst {n}.{i}");
        }
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

fn hwm_kb(out: &str) -> u64 {
    out.lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        .expect("VmHWM")
}

#[test]
fn a_slow_disk_never_stalls_the_tree_or_its_shutdown() {
    let dir = scratch("slow-disk");
    let running = spawn(
        SCENARIO,
        &[
            ("RT_DIR", dir.to_str().unwrap()),
            ("RT_SLOW_MS", "200"),
            ("RT_BURST", "5"),
        ],
    );
    std::thread::sleep(Duration::from_millis(2000));
    running.signal(libc::SIGTERM);
    let ran = running.wait(Duration::from_secs(8));
    // 50 ticks a second for 2 s: the tree didn't wait on the disk.
    assert!(ticks(&ran.stdout) >= 80, "the tree stalled: {ran:?}");
    assert!(
        !ran.timed_out && ran.code == Some(0),
        "shutdown stalled: {ran:?}"
    );
    // It gave up waiting for the writer after END_WAIT, and said so.
    assert!(ran.stderr.contains("END may be missing"), "{}", ran.stderr);
}

#[test]
fn a_failing_disk_is_reported_once_and_the_tree_carries_on() {
    let dir = scratch("failing-disk");
    let ran = child(
        SCENARIO,
        &[
            ("RT_DIR", dir.to_str().unwrap()),
            ("RT_FAIL", "1"),
            ("RT_STOP", "25"),
            ("RT_BURST", "3"),
        ],
        Duration::from_secs(10),
    );
    assert!(ran.code == Some(0), "{ran:?}");
    assert_eq!(ticks(&ran.stdout), 25, "{ran:?}");
    let said = ran
        .stderr
        .matches("can't write events.log: injected failure")
        .count();
    assert_eq!(said, 1, "{}", ran.stderr);
}

#[test]
fn a_flood_of_records_is_dropped_visibly_with_bounded_memory() {
    let dir = scratch("flood");
    let ran = child(
        SCENARIO,
        &[
            ("RT_DIR", dir.to_str().unwrap()),
            ("RT_SLOW_MS", "1"),
            ("RT_BURST", "100"),
            ("RT_STOP", "50"),
        ],
        Duration::from_secs(15),
    );
    assert!(ran.code == Some(0), "{ran:?}");
    let text = read(runs(&dir).pop().unwrap().join("events.log"));
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        text.contains("lines dropped here: the recorder fell behind"),
        "no drop note"
    );
    let end = lines.last().unwrap();
    assert!(
        end.contains(" END SIGTERM") && end.contains("lines dropped)"),
        "{end}"
    );
    // Whatever was accepted is in order, before END.
    let bursts: Vec<&str> = lines
        .iter()
        .filter(|l| l.contains(" burst "))
        .copied()
        .collect();
    assert!(!bursts.is_empty());
    let order: Vec<(u64, u64)> = bursts
        .iter()
        .filter_map(|l| {
            let (n, i) = l.rsplit_once(" burst ")?.1.split_once('.')?;
            Some((n.parse().ok()?, i.parse().ok()?))
        })
        .collect();
    assert!(
        order.windows(2).all(|w| w[0] < w[1]),
        "records out of order"
    );
    assert!(
        hwm_kb(&ran.stdout) < 64 * 1024,
        "{} kB",
        hwm_kb(&ran.stdout)
    );
}

/// An old run folder that ended cleanly, last touched `age_days` ago.
fn old_run(dir: &Path, name: &str, age_days: u64) -> PathBuf {
    let run = dir.join(name);
    std::fs::create_dir_all(&run).unwrap();
    std::fs::write(
        run.join("events.log"),
        "2000-01-01 00:00:00.000 START t 0.1.0 on h (pid 1, UTC)\n\
         2000-01-01 00:00:05.000 END SIGTERM, after 5s\n",
    )
    .unwrap();
    backdate(&run, age_days);
    run
}

fn backdate(path: &Path, age_days: u64) {
    let when = std::time::SystemTime::now() - Duration::from_secs(age_days * 86_400 + 60);
    std::fs::File::open(path)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

#[test]
fn old_runs_go_but_never_a_running_one_or_anything_else() {
    let dir = scratch("retention");
    // Oldest first: day 20 down to day 10.
    let unrelated = old_run(&dir, "2000-01-01_09-00-00", 20);
    std::fs::write(unrelated.join("notes.txt"), "mine").unwrap();
    backdate(&unrelated, 20);
    let olds: Vec<PathBuf> = (1..=6)
        .map(|i| old_run(&dir, &format!("2000-01-0{i}_00-00-00"), 16 - i as u64))
        .collect();
    std::fs::create_dir_all(dir.join("photos")).unwrap();
    // A tree still running, its folder made to look oldest of all.
    let env = [("RT_DIR", dir.to_str().unwrap())];
    let first = spawn(SCENARIO, &env);
    std::thread::sleep(Duration::from_millis(800));
    let running = runs(&dir)
        .into_iter()
        .find(|r| !r.starts_with(&unrelated) && !olds.contains(r) && r.join("events.log").is_file())
        .expect("the running tree's folder");
    backdate(&running, 30);
    // keep_runs = 3: this run and the 2 newest others.
    let ran = child(
        SCENARIO,
        &[env[0], ("RT_KEEP_RUNS", "3"), ("RT_STOP", "3")],
        Duration::from_secs(10),
    );
    first.signal(libc::SIGTERM);
    let _ = first.wait(Duration::from_secs(10));
    assert!(ran.code == Some(0), "{ran:?}");
    assert!(running.is_dir(), "a running tree's folder was deleted");
    assert!(
        unrelated.join("notes.txt").is_file(),
        "a folder with other files was deleted"
    );
    assert!(
        dir.join("photos").is_dir(),
        "a folder that isn't a run was deleted"
    );
    // Past the limit, every plain old run went: what couldn't go (the
    // running one, the one with notes.txt) and this run make the 3.
    assert!(olds.iter().all(|r| !r.exists()), "{:?}", runs(&dir));
    assert_eq!(runs(&dir).len(), 4, "3 runs and photos: {:?}", runs(&dir));
    // The run that did it says so (found by its note: when the running
    // tree stopped, its folder became the newest).
    let said = runs(&dir)
        .iter()
        .filter_map(|r| std::fs::read_to_string(r.join("events.log")).ok())
        .any(|t| t.contains("removed 6 old run folder(s)"));
    assert!(said, "no run said what it removed");
}

#[test]
fn runs_older_than_keep_days_go() {
    let dir = scratch("keep-days");
    let old = old_run(&dir, "2000-01-01_00-00-00", 40);
    let recent = old_run(&dir, "2000-01-02_00-00-00", 2);
    let ran = child(
        SCENARIO,
        &[
            ("RT_DIR", dir.to_str().unwrap()),
            ("RT_KEEP_DAYS", "30"),
            ("RT_KEEP_RUNS", "0"),
            ("RT_STOP", "3"),
        ],
        Duration::from_secs(10),
    );
    assert!(ran.code == Some(0), "{ran:?}");
    assert!(!old.exists() && recent.is_dir());
}

#[test]
fn a_file_past_max_file_kb_moves_aside_and_the_new_one_ends_with_end() {
    let dir = scratch("rotate");
    let ran = child(
        SCENARIO,
        &[
            ("RT_DIR", dir.to_str().unwrap()),
            ("RT_MAX_FILE_KB", "4"),
            ("RT_BURST", "20"),
            ("RT_STOP", "20"),
        ],
        Duration::from_secs(10),
    );
    assert!(ran.code == Some(0), "{ran:?}");
    let run = runs(&dir).pop().unwrap();
    let old = std::fs::metadata(run.join("events.1.log"))
        .expect("events.1.log")
        .len();
    let now = read(run.join("events.log"));
    assert!(old <= 5 * 1024, "events.1.log is {old} bytes");
    assert!(now.len() <= 5 * 1024, "events.log is {} bytes", now.len());
    assert!(now.contains("(continued from events.1.log"), "{now}");
    assert!(
        now.lines().last().unwrap().contains(" END SIGTERM"),
        "{now}"
    );
}

/// Wait for a file to appear (another process's signal).
fn await_file(path: &Path, within: Duration) {
    let started = std::time::Instant::now();
    while !path.exists() {
        assert!(
            started.elapsed() < within,
            "{} never appeared",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Run folders being made (`.new-*`), left in `dir`.
fn half_made(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with(".new-"))
                })
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn a_run_being_made_is_never_pruned_by_another_tree() {
    let dir = scratch("init-race");
    let sync = scratch("init-race-sync").join("a");
    let olds: Vec<PathBuf> = (1..=3)
        .map(|i| old_run(&dir, &format!("2000-01-0{i}_00-00-00"), 10 - i as u64))
        .collect();
    // A stops between making its folder and locking it.
    let a = spawn(
        SCENARIO,
        &[
            ("RT_DIR", dir.to_str().unwrap()),
            ("BONSAI_RT_PAUSE", sync.to_str().unwrap()),
        ],
    );
    await_file(&sync.with_extension("paused"), Duration::from_secs(10));
    assert_eq!(half_made(&dir).len(), 1, "A's folder, not yet a run");
    // B wants to keep 1 run: while A is paused, it waits its turn.
    let b = spawn(
        SCENARIO,
        &[("RT_DIR", dir.to_str().unwrap()), ("RT_KEEP_RUNS", "1")],
    );
    std::thread::sleep(Duration::from_millis(1000));
    assert!(
        olds.iter().all(|r| r.is_dir()),
        "B pruned while A held the folder's lock"
    );
    std::fs::write(sync.with_extension("go"), "").unwrap();
    // A finishes making its run; then B prunes, around it.
    std::thread::sleep(Duration::from_millis(1500));
    b.signal(libc::SIGTERM);
    let b = b.wait(Duration::from_secs(10));
    a.signal(libc::SIGTERM);
    let a = a.wait(Duration::from_secs(10));
    assert!(a.code == Some(0) && b.code == Some(0), "{a:?}\n{b:?}");
    assert!(
        olds.iter().all(|r| !r.exists()),
        "the old runs past keep_runs stay"
    );
    assert!(half_made(&dir).is_empty(), "{:?}", half_made(&dir));
    let left = runs(&dir);
    assert_eq!(left.len(), 2, "A's run and B's: {left:?}");
    let texts: Vec<String> = left.iter().map(|r| read(r.join("events.log"))).collect();
    assert!(
        texts.iter().all(|t| t.contains(" END SIGTERM")),
        "{texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|t| t.contains("removed 3 old run folder(s)")),
        "{texts:?}"
    );
}

#[test]
fn without_the_folder_lock_a_run_records_nothing_and_prunes_nothing() {
    let dir = scratch("no-dir-lock");
    let old = old_run(&dir, "2000-01-01_00-00-00", 10);
    // The lock file can't be opened: it's a folder.
    std::fs::create_dir_all(dir.join(".bonsai-record.lock")).unwrap();
    let ran = child(
        SCENARIO,
        &[
            ("RT_DIR", dir.to_str().unwrap()),
            ("RT_KEEP_RUNS", "1"),
            ("RT_STOP", "3"),
        ],
        Duration::from_secs(10),
    );
    assert!(ran.code == Some(0), "the tree itself runs on: {ran:?}");
    assert!(old.is_dir(), "pruned without the folder's lock");
    let made: Vec<PathBuf> = runs(&dir)
        .into_iter()
        .filter(|r| *r != old && !r.ends_with(".bonsai-record.lock"))
        .collect();
    assert!(made.is_empty(), "made without the folder's lock: {made:?}");
    assert!(half_made(&dir).is_empty(), "{:?}", half_made(&dir));
    assert!(
        ran.stderr
            .contains("bonsai: no run logs this time: can't open"),
        "{}",
        ran.stderr
    );
}

#[test]
fn a_tree_that_waits_too_long_for_the_folder_makes_nothing_there() {
    let dir = scratch("lock-timeout");
    let sync = scratch("lock-timeout-sync");
    let (a_at, b_at) = (sync.join("a"), sync.join("b"));
    let olds: Vec<PathBuf> = (1..=3)
        .map(|i| old_run(&dir, &format!("2000-01-0{i}_00-00-00"), 10 - i as u64))
        .collect();
    // A holds the folder's lock, paused after making its folder; it will
    // prune down to 1 run.
    let a = spawn(
        SCENARIO,
        &[
            ("RT_DIR", dir.to_str().unwrap()),
            ("RT_KEEP_RUNS", "1"),
            ("BONSAI_RT_PAUSE", a_at.to_str().unwrap()),
        ],
    );
    await_file(&a_at.with_extension("paused"), Duration::from_secs(10));
    // B waits for the lock longer than DIR_LOCK_WAIT. Were it to make a
    // folder anyway, it would pause there too, for A to prune under it.
    let b = spawn(
        SCENARIO,
        &[
            ("RT_DIR", dir.to_str().unwrap()),
            ("BONSAI_RT_PAUSE", b_at.to_str().unwrap()),
        ],
    );
    std::thread::sleep(crate::bonsai::record::DIR_LOCK_WAIT + Duration::from_millis(1500));
    assert!(
        !b_at.with_extension("paused").exists(),
        "B made a folder without the folder's lock"
    );
    assert_eq!(half_made(&dir).len(), 1, "only A's: {:?}", half_made(&dir));
    // A goes on and prunes; B, had it paused, is let go too.
    std::fs::write(a_at.with_extension("go"), "").unwrap();
    std::fs::write(b_at.with_extension("go"), "").unwrap();
    std::thread::sleep(Duration::from_millis(1000));
    b.signal(libc::SIGTERM);
    let b = b.wait(Duration::from_secs(10));
    a.signal(libc::SIGTERM);
    let a = a.wait(Duration::from_secs(10));
    assert!(a.code == Some(0) && b.code == Some(0), "{a:?}\n{b:?}");
    assert!(
        b.stderr
            .contains("bonsai: no run logs this time: another tree held"),
        "B says why it keeps nothing: {}",
        b.stderr
    );
    assert!(olds.iter().all(|r| !r.exists()), "A pruned as asked");
    assert!(half_made(&dir).is_empty(), "{:?}", half_made(&dir));
    let left = runs(&dir);
    assert_eq!(left.len(), 1, "A's run only: {left:?}");
    let text = read(left[0].join("events.log"));
    assert!(text.contains(" END SIGTERM"), "{text}");
}

#[test]
fn a_tree_killed_while_making_its_run_leaves_nothing_in_the_way() {
    let dir = scratch("init-killed");
    let sync = scratch("init-killed-sync").join("a");
    let old = old_run(&dir, "2000-01-01_00-00-00", 10);
    let a = spawn(
        SCENARIO,
        &[
            ("RT_DIR", dir.to_str().unwrap()),
            ("BONSAI_RT_PAUSE", sync.to_str().unwrap()),
        ],
    );
    await_file(&sync.with_extension("paused"), Duration::from_secs(10));
    a.signal(libc::SIGKILL);
    let _ = a.wait(Duration::from_secs(5));
    assert_eq!(half_made(&dir).len(), 1);
    // The next tree gets the folder's lock (the OS let go of A's), clears
    // A's half-made folder, and prunes as asked.
    let ran = child(
        SCENARIO,
        &[
            ("RT_DIR", dir.to_str().unwrap()),
            ("RT_KEEP_RUNS", "1"),
            ("RT_STOP", "3"),
        ],
        Duration::from_secs(10),
    );
    assert!(ran.code == Some(0), "{ran:?}");
    assert!(half_made(&dir).is_empty(), "{:?}", half_made(&dir));
    assert!(!old.exists());
    assert_eq!(runs(&dir).len(), 1);
}

/// The `record` row a running tree serves to `bonsai top`, once it says
/// something other than `starting`.
fn record_row(port: u16) -> String {
    use std::io::{BufRead, BufReader};
    let started = std::time::Instant::now();
    loop {
        assert!(started.elapsed() < Duration::from_secs(10), "no record row");
        let Ok(stream) = std::net::TcpStream::connect(("127.0.0.1", port)) else {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { break };
            if line.starts_with("record\t") && !line.starts_with("record\tstarting") {
                return line;
            }
        }
    }
}

#[test]
fn bonsai_top_hears_whether_the_run_is_being_recorded() {
    use super::support::free_port;
    let dir = scratch("top-row");
    let port = free_port().to_string();
    let top = format!("127.0.0.1:{port}");
    let a = spawn(
        SCENARIO,
        &[("RT_DIR", dir.to_str().unwrap()), ("BONSAI_TOP", &top)],
    );
    let row = record_row(port.parse().unwrap());
    a.signal(libc::SIGTERM);
    let _ = a.wait(Duration::from_secs(10));
    let folder = runs(&dir).pop().unwrap();
    assert_eq!(row, format!("record\ton\t{}", folder.display()));

    // Unavailable, and why.
    let dir = scratch("top-row-locked");
    std::fs::create_dir_all(dir.join(".bonsai-record.lock")).unwrap();
    let b = spawn(
        SCENARIO,
        &[("RT_DIR", dir.to_str().unwrap()), ("BONSAI_TOP", &top)],
    );
    let row = record_row(port.parse().unwrap());
    b.signal(libc::SIGTERM);
    let _ = b.wait(Duration::from_secs(10));
    assert!(row.starts_with("record\tunavailable\tcan't open "), "{row}");
}

/// The `record` rows a running tree serves to `bonsai top`, up to and
/// including the first `done` accepts.
fn record_rows_until(port: u16, done: impl Fn(&str) -> bool) -> Vec<String> {
    use std::io::{BufRead, BufReader};
    let started = std::time::Instant::now();
    let mut rows = Vec::new();
    loop {
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "no such row: {rows:?}"
        );
        let Ok(stream) = std::net::TcpStream::connect(("127.0.0.1", port)) else {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        };
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { break };
            if line.starts_with("record\t") {
                rows.push(line.clone());
                if done(&line) {
                    return rows;
                }
            }
        }
    }
}

/// The scenario, running with `env` and BONSAI_TOP on a free port: the
/// tree, its port, and the folder its runs go in.
fn health_tree(name: &str, env: &[(&str, &str)]) -> (super::support::Running, u16, PathBuf) {
    let dir = scratch(name);
    let port = super::support::free_port();
    let top = format!("127.0.0.1:{port}");
    let mut all = vec![
        ("RT_DIR", dir.to_str().unwrap()),
        ("BONSAI_TOP", top.as_str()),
    ];
    all.extend_from_slice(env);
    (spawn(SCENARIO, &all), port, dir)
}

/// Stop it; it carried on throughout, whatever its run logs did.
fn stop_tree(tree: super::support::Running) {
    tree.signal(libc::SIGTERM);
    let ran = tree.wait(Duration::from_secs(10));
    assert_eq!(ran.code, Some(0), "the tree didn't carry on: {ran:?}");
}

/// Wait until `path` holds `text`.
fn await_text(path: &Path, text: &str) {
    let started = std::time::Instant::now();
    while !std::fs::read_to_string(path).is_ok_and(|t| t.contains(text)) {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "no {text:?} in {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Run the scenario with `env` until its first record row `done` accepts;
/// the rows up to there, and its run folder.
fn rows_of(
    name: &str,
    env: &[(&str, &str)],
    done: impl Fn(&str) -> bool,
) -> (Vec<String>, PathBuf) {
    let (tree, port, dir) = health_tree(name, env);
    let rows = record_rows_until(port, done);
    stop_tree(tree);
    (rows, runs(&dir).pop().unwrap_or_default())
}

#[test]
fn run_logs_are_on_only_once_every_file_is_open() {
    // Slow writes hold START up: meanwhile it's starting, not on.
    let (rows, folder) = rows_of(
        "health-on",
        &[("RT_KINDS", "events,panics"), ("RT_SLOW_MS", "700")],
        |r| !r.starts_with("record\tstarting"),
    );
    assert!(rows[0].starts_with("record\tstarting\t"), "{rows:?}");
    assert_eq!(
        rows.last().unwrap(),
        &format!("record\ton\t{}", folder.display())
    );
}

#[test]
fn run_logs_none_of_which_open_are_unavailable_and_say_why() {
    let (rows, folder) = rows_of(
        "health-none",
        &[("RT_KINDS", "events,panics"), ("RT_OPEN_FAIL", "3")],
        |r| !r.starts_with("record\tstarting"),
    );
    assert_eq!(
        rows.last().unwrap(),
        &format!(
            "record\tunavailable\tnothing can be written in {}\tevents\tcan't open events.log: injected failure\tpanics\tcan't open panics.log: injected failure",
            folder.display()
        )
    );
}

#[test]
fn run_logs_one_of_which_fails_to_open_are_partial_and_the_rest_carry_on() {
    let (tree, port, dir) = health_tree(
        "health-partial",
        &[("RT_KINDS", "events,errors"), ("RT_OPEN_FAIL", "4")],
    );
    let rows = record_rows_until(port, |r| !r.starts_with("record\tstarting"));
    let folder = runs(&dir).pop().unwrap();
    assert_eq!(
        rows.last().unwrap(),
        &format!(
            "record\tpartial\t{}\terrors\tcan't open errors.log: injected failure",
            folder.display()
        )
    );
    // The file that opened is written as usual.
    await_text(&folder.join("events.log"), "launch 2");
    stop_tree(tree);
    let events = read(folder.join("events.log"));
    assert!(events.contains(" END SIGTERM"), "{events}");
    assert!(!folder.join("errors.log").exists());
}

#[test]
fn a_file_that_fails_later_turns_on_into_partial() {
    let scratchpad = scratch("health-later-when");
    let when = scratchpad.join("fail-now");
    let (tree, port, dir) = health_tree(
        "health-later",
        &[
            ("RT_KINDS", "events,panics"),
            ("RT_WRITE_FAIL", "1"),
            ("RT_WRITE_FAIL_WHEN", when.to_str().unwrap()),
            ("RT_BURST", "1"),
        ],
    );
    let on = record_rows_until(port, |r| r.starts_with("record\ton\t"));
    let folder = runs(&dir).pop().unwrap();
    assert_eq!(
        on.last().unwrap(),
        &format!("record\ton\t{}", folder.display())
    );
    // Now every write to events.log fails.
    std::fs::write(&when, "").unwrap();
    let rows = record_rows_until(port, |r| r.starts_with("record\tpartial"));
    assert_eq!(
        rows.last().unwrap(),
        &format!(
            "record\tpartial\t{}\tevents\tcan't write events.log: injected failure",
            folder.display()
        )
    );
    stop_tree(tree);
    // The other file goes on to the end.
    assert!(read(folder.join("panics.log")).contains(" END SIGTERM"));
}

#[test]
fn run_logs_turned_off_say_so() {
    let (rows, _) = rows_of("health-off", &[("BONSAI_RECORD", "off")], |r| {
        r.starts_with("record\toff")
    });
    assert_eq!(rows.last().unwrap(), "record\toff\tBONSAI_RECORD=off");
}

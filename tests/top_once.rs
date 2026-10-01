//! `bonsai top --once` end to end: the real binary, against a stand-in for a
//! tree's top server that sends what the runtime sends (the rows are the
//! runtime's own, as its `a_snapshot_renders_as_tab_separated_rows` test
//! renders them), for a recorder writing some files and not others.

use std::io::Write;
use std::net::TcpListener;
use std::process::Command;
use std::time::Duration;

const BONSAI: &str = env!("CARGO_BIN_EXE_bonsai");

/// Serve snapshots whose uptime moves 1 s each, with `record` as the
/// record row, to every client, until the test ends.
fn serve(record: &'static str) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for client in listener.incoming() {
            let Ok(mut client) = client else { continue };
            std::thread::spawn(move || {
                for n in 1u64.. {
                    let snapshot = format!(
                        "bonsai-top 1\t{}\t{}\t40\t0\n\
                         branch\tsensor\t{n}\t{n}\t0\t12\t5\t0\t0\n\
                         edge\tnet\tup\t1\t2\t0\t0\t\t3\t0\t0\n\
                         {record}\n\
                         end\n",
                        n * 1000,
                        n * 2
                    );
                    if client.write_all(snapshot.as_bytes()).is_err() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            });
        }
    });
    port
}

fn top_once(port: u16) -> String {
    let dir = std::env::temp_dir().join(format!("bonsai-top-once-{}-{port}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = Command::new(BONSAI)
        .args(["top", "local", "--port", &port.to_string(), "--once"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn a_recorder_writing_some_files_and_not_others_is_shown_as_such() {
    let port =
        serve("record\tpartial\tlogs/2026-10-01_10-00-00\terrors\tcan't open errors.log: denied");
    let out = top_once(port);
    let line = out.lines().nth(1).unwrap_or_default();
    assert_eq!(
        line,
        "run logs: partly, in logs/2026-10-01_10-00-00; not written: errors (can't open errors.log: denied)",
        "{out}"
    );
}

#[test]
fn a_recorder_writing_nothing_says_why_for_each_file() {
    let port = serve(
        "record\tunavailable\tnothing can be written in logs/r1\tevents\tcan't open events.log: denied\tpanics\tcan't open panics.log: denied",
    );
    let out = top_once(port);
    assert!(
        out.contains(
            "run logs: unavailable: nothing can be written in logs/r1: events (can't open events.log: denied), panics (can't open panics.log: denied)"
        ),
        "{out}"
    );
}

#[test]
fn a_tree_older_than_the_record_row_is_still_read() {
    let port = serve("log\t14:05:03.123Z  INFO sensor: 26.5 °C");
    let out = top_once(port);
    assert!(out.contains("run logs: not reported by this tree"), "{out}");
    assert!(out.contains("sensor"), "{out}");
}

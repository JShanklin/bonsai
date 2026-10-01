//! `bonsai top`: a live view of a running tree. The tree serves its stats on
//! 127.0.0.1:7777 (`BONSAI_TOP`, in its `src/bonsai.rs`); top reads them there,
//! or on a Pi through `ssh -W`, which needs nothing on the Pi but sshd.
//! The live view is in `view` (its tabs) and `graph` (the node graph).

mod graph;
mod view;

use std::io::{self, BufRead, BufReader};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The port a tree serves top on unless its `BONSAI_TOP` says otherwise.
pub const PORT: u16 = 7777;
/// Rates are taken over this long, so a branch ticking once a second doesn't
/// flicker between 0 and 2 from one half-second report to the next.
const WINDOW_MS: u64 = 2000;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Branch {
    pub name: String,
    pub inputs: u64,
    pub sent: u64,
    pub panics: u64,
    pub busy_us: u64,
    pub max_us: u64,
    /// Out of service: its setup panicked (false from an older tree).
    pub failed: bool,
    /// Inputs dropped while out of service.
    pub discarded: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Edge {
    pub name: String,
    pub state: String,
    pub received: u64,
    pub sent: u64,
    pub dropped: u64,
    pub restarts: u64,
    pub error: String,
    /// Taken into the edge's queue (0 from a tree older than this count).
    pub accepted: u64,
    /// Taken, but `execute` failed on it.
    pub failed: u64,
    /// Taken, but a copy couldn't be delivered (a slow TCP client).
    pub discarded: u64,
}

impl Edge {
    /// Taken into the edge, then never delivered: failed or discarded.
    pub fn lost(&self) -> u64 {
        self.failed + self.discarded
    }
}

/// A link in bonsai.toml, and how many deliveries it has carried.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Link {
    pub from: String,
    /// The message; empty when an edge is at one end.
    pub label: String,
    pub to: Vec<String>,
    pub count: u64,
}

/// The tree's process and its computer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sys {
    pub rss_kb: u64,
    pub cpu_ms: u64,
    pub threads: u64,
    /// The one-minute load average, times 100.
    pub load: u64,
    pub mem_total_kb: u64,
    pub mem_available_kb: u64,
}

/// What the tree's recorder (its run logs) is doing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Record {
    /// `off`, `starting`, `on`, `partial` or `unavailable`.
    pub state: String,
    /// The run's folder (on, partial), or why not.
    pub detail: String,
    /// Each kind asked for that isn't being written, and why (none from a
    /// tree older than this list).
    pub failed: Vec<(String, String)>,
}

/// One report from the tree: its counts, and the log lines since the last.
/// A tree from before `link` and `sys` rows sends none of either, and one
/// from before `record` rows none of those.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub uptime_ms: u64,
    pub events: u64,
    pub max_event_us: u64,
    pub inbox: u64,
    pub branches: Vec<Branch>,
    pub edges: Vec<Edge>,
    pub links: Vec<Link>,
    pub sys: Option<Sys>,
    pub record: Option<Record>,
    pub logs: Vec<String>,
}

/// Read one snapshot (through its `end` line); None at the end of the stream.
pub fn read_snapshot(r: &mut impl BufRead) -> io::Result<Option<Snapshot>> {
    let mut lines = Vec::new();
    loop {
        let mut line = String::new();
        if r.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end_matches(['\n', '\r']).to_string();
        if line == "end" {
            break;
        }
        lines.push(line);
    }
    parse(&lines).map(Some)
}

fn bad(what: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.into())
}

/// A snapshot from its lines (without `end`).
pub fn parse(lines: &[String]) -> io::Result<Snapshot> {
    let mut s = Snapshot::default();
    let num = |f: Option<&str>| -> io::Result<u64> {
        f.and_then(|f| f.parse().ok())
            .ok_or_else(|| bad("a number is missing"))
    };
    for (i, line) in lines.iter().enumerate() {
        let mut f = line.split('\t');
        match f.next() {
            Some(head) if i == 0 && head.starts_with("bonsai-top ") => {
                if head != "bonsai-top 1" {
                    return Err(bad(
                        "the tree speaks a newer top than this bonsai: update bonsai",
                    ));
                }
                s.uptime_ms = num(f.next())?;
                s.events = num(f.next())?;
                s.max_event_us = num(f.next())?;
                s.inbox = num(f.next())?;
            }
            _ if i == 0 => return Err(bad("that's not a bonsai tree's top server")),
            Some("branch") => s.branches.push(Branch {
                name: f.next().unwrap_or_default().to_string(),
                inputs: num(f.next())?,
                sent: num(f.next())?,
                panics: num(f.next())?,
                busy_us: num(f.next())?,
                max_us: num(f.next())?,
                // Newer trees add these; older ones don't.
                failed: f.next() == Some("1"),
                discarded: f.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            }),
            Some("edge") => s.edges.push(Edge {
                name: f.next().unwrap_or_default().to_string(),
                state: f.next().unwrap_or_default().to_string(),
                received: num(f.next())?,
                sent: num(f.next())?,
                dropped: num(f.next())?,
                restarts: num(f.next())?,
                error: f.next().unwrap_or_default().to_string(),
                // Newer trees add these after the error; older ones don't.
                accepted: f.next().and_then(|v| v.parse().ok()).unwrap_or(0),
                failed: f.next().and_then(|v| v.parse().ok()).unwrap_or(0),
                discarded: f.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            }),
            Some("link" | "wire") => s.links.push(Link {
                from: f.next().unwrap_or_default().to_string(),
                label: f.next().unwrap_or_default().to_string(),
                to: f
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .filter(|t| !t.is_empty())
                    .map(String::from)
                    .collect(),
                count: num(f.next())?,
            }),
            Some("sys") => {
                s.sys = Some(Sys {
                    rss_kb: num(f.next())?,
                    cpu_ms: num(f.next())?,
                    threads: num(f.next())?,
                    load: num(f.next())?,
                    mem_total_kb: num(f.next())?,
                    mem_available_kb: num(f.next())?,
                })
            }
            Some("record") => {
                let state = f.next().unwrap_or_default().to_string();
                let detail = f.next().unwrap_or_default().to_string();
                // Then kind, why; kind, why; …
                let rest: Vec<&str> = f.collect();
                let failed = rest
                    .chunks(2)
                    .map(|p| (p[0].to_string(), p.get(1).unwrap_or(&"").to_string()))
                    .collect();
                s.record = Some(Record {
                    state,
                    detail,
                    failed,
                })
            }
            Some("log") => s.logs.push(line["log\t".len()..].to_string()),
            _ => {} // a row kind from a newer tree: skip it
        }
    }
    Ok(s)
}

// ---------------------------------------------------------------------------
// Why a part is quiet: from the counts, never guessing past them
// ---------------------------------------------------------------------------

/// How a branch, edge or the recorder is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    /// Working as far as the counts show.
    Ok,
    /// Nothing seen yet: not a fault, but worth knowing.
    Quiet,
    /// Something's wrong: out of service, retrying, losing messages.
    Problem,
}

/// What a branch is doing, or why it's quiet, as far as the counts say.
pub fn branch_status(b: &Branch, s: &Snapshot) -> (Health, String) {
    if b.failed {
        return (
            Health::Problem,
            format!(
                "out of service: its setup panicked; set up again on a later input ({} inputs dropped)",
                b.discarded
            ),
        );
    }
    if b.inputs > 0 {
        return (Health::Ok, "running".to_string());
    }
    // A tree from before link rows can't say what feeds a branch.
    if s.links.is_empty() && s.sys.is_none() {
        return (Health::Quiet, "no input observed".to_string());
    }
    let feeding: Vec<&Link> = s.links.iter().filter(|l| l.to.contains(&b.name)).collect();
    if feeding.is_empty() {
        return (
            Health::Quiet,
            "no input observed; nothing is linked to it, so only a rate would feed it".to_string(),
        );
    }
    let from: Vec<&str> = feeding.iter().map(|l| l.from.as_str()).collect();
    (
        Health::Quiet,
        format!(
            "no input observed; nothing has come over its links yet (from {})",
            from.join(", ")
        ),
    )
}

/// What an edge is doing, and what it's lost, as far as the counts say.
pub fn edge_status(e: &Edge) -> (Health, String) {
    let mut health = Health::Ok;
    let mut parts = Vec::new();
    match e.state.as_str() {
        "retrying" => {
            health = Health::Problem;
            parts.push(if e.error.is_empty() {
                "retrying".to_string()
            } else {
                format!("retrying: {}", e.error)
            });
        }
        "up" => parts.push("up".to_string()),
        _ => {
            health = Health::Quiet;
            parts.push("connecting: not up yet".to_string());
        }
    }
    if e.dropped > 0 {
        health = Health::Problem;
        parts.push(format!("{} dropped: its queue was full", e.dropped));
    }
    if e.lost() > 0 {
        health = Health::Problem;
        parts.push(format!(
            "{} lost ({} failed to send, {} not delivered)",
            e.lost(),
            e.failed,
            e.discarded
        ));
    }
    if health == Health::Ok && e.received == 0 && e.sent == 0 {
        health = Health::Quiet;
        parts.push("no traffic observed".to_string());
    }
    if e.state == "up" && !e.error.is_empty() {
        parts.push(format!("last error: {}", e.error));
    }
    (health, parts.join("; "))
}

/// The recorder's state, in a line.
pub fn record_status(s: &Snapshot) -> (Health, String) {
    let Some(r) = &s.record else {
        return (
            Health::Quiet,
            "run logs: not reported by this tree (`bonsai sync` brings its runtime up to date)"
                .to_string(),
        );
    };
    let detail = |what: &str| {
        if r.detail.is_empty() {
            what.to_string()
        } else {
            format!("{what} ({})", r.detail)
        }
    };
    let failed = r
        .failed
        .iter()
        .map(|(kind, why)| format!("{kind} ({why})"))
        .collect::<Vec<_>>()
        .join(", ");
    match r.state.as_str() {
        "on" => (Health::Ok, format!("run logs: on, in {}", r.detail)),
        "partial" => (
            Health::Problem,
            format!("run logs: partly, in {}; not written: {failed}", r.detail),
        ),
        "unavailable" if !failed.is_empty() => (
            Health::Problem,
            format!("run logs: unavailable: {}: {failed}", r.detail),
        ),
        "starting" => (Health::Quiet, format!("run logs: {}", detail("starting"))),
        "unavailable" => (
            Health::Problem,
            format!("run logs: unavailable: {}", r.detail),
        ),
        _ => (Health::Quiet, format!("run logs: {}", detail("off"))),
    }
}

/// Per second, from two counts `ms` apart.
pub fn per_sec(before: u64, after: u64, ms: u64) -> f64 {
    if ms == 0 {
        return 0.0;
    }
    after.saturating_sub(before) as f64 * 1000.0 / ms as f64
}

/// `1h02m`, `3m04s`, `12s`.
pub fn uptime(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m{:02}s", s / 60, s % 60),
        _ => format!("{}h{:02}m", s / 3600, s / 60 % 60),
    }
}

/// A time: `850 µs`, `12.4 ms`, `1.2 s`.
pub fn took(us: u64) -> String {
    match us {
        0..10_000 => format!("{us} µs"),
        10_000..1_000_000 => format!("{:.1} ms", us as f64 / 1000.0),
        _ => format!("{:.1} s", us as f64 / 1_000_000.0),
    }
}

/// Where the tree is: `None` for this computer, or an ssh destination.
pub fn destination(arg: Option<&str>, env: Option<&str>, config: Option<&str>) -> Option<String> {
    match arg {
        Some("local" | "localhost") => return None,
        Some(host) => return Some(host.to_string()),
        None => {}
    }
    if let Some(pi) = env.filter(|p| !p.is_empty()) {
        return Some(pi.to_string());
    }
    // A tree that builds for a Pi runs there.
    let config = config?;
    crate::parse_target(config)?;
    crate::parse_scoped_key(config, "[env]", "BONSAI_PI")
}

// ---------------------------------------------------------------------------
// Connecting
// ---------------------------------------------------------------------------

pub(crate) enum Update {
    Snapshot(Box<Snapshot>),
    Down(String),
}

impl From<Snapshot> for Update {
    fn from(s: Snapshot) -> Self {
        Update::Snapshot(Box::new(s))
    }
}

/// A connection to a tree: its reader, and the ssh process behind it.
fn connect(
    dest: &Option<String>,
    port: u16,
) -> io::Result<(Box<dyn BufRead + Send>, Option<Child>)> {
    match dest {
        None => {
            let stream = TcpStream::connect(("127.0.0.1", port)).map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!("no tree on 127.0.0.1:{port} ({e}): is it running?"),
                )
            })?;
            Ok((Box::new(BufReader::new(stream)), None))
        }
        Some(host) => {
            let mut child = Command::new("ssh")
                .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=5", "-W"])
                .arg(format!("127.0.0.1:{port}"))
                .arg(host)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| io::Error::new(e.kind(), format!("can't run ssh: {e}")))?;
            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| bad("ssh gave no output"))?;
            Ok((Box::new(BufReader::new(stdout)), Some(child)))
        }
    }
}

/// Why an ssh connection ended, from what it printed.
fn ssh_error(child: &mut Child) -> Option<String> {
    use std::io::Read;
    let _ = child.kill();
    let _ = child.wait();
    let mut err = String::new();
    child.stderr.take()?.read_to_string(&mut err).ok()?;
    let err = err.trim();
    let last = err.lines().last()?;
    Some(
        if last.contains("stdio forwarding failed") || last.contains("open failed") {
            "no tree answered there: is it running?".to_string()
        } else {
            last.to_string()
        },
    )
}

/// Read snapshots, reconnecting every second, until the receiver is gone.
fn reader(
    dest: Option<String>,
    port: u16,
    tx: mpsc::Sender<Update>,
    ssh: Arc<Mutex<Option<Child>>>,
) {
    loop {
        let why = match connect(&dest, port) {
            Err(e) => e.to_string(),
            Ok((mut r, child)) => {
                *ssh.lock().unwrap_or_else(|e| e.into_inner()) = child;
                let why = loop {
                    match read_snapshot(&mut r) {
                        Ok(Some(s)) => {
                            if tx.send(Update::from(s)).is_err() {
                                return;
                            }
                        }
                        Ok(None) => break "the tree stopped".to_string(),
                        Err(e) => break e.to_string(),
                    }
                };
                let child = ssh.lock().unwrap_or_else(|e| e.into_inner()).take();
                child.and_then(|mut c| ssh_error(&mut c)).unwrap_or(why)
            }
        };
        if tx.send(Update::Down(why)).is_err() {
            return;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

// ---------------------------------------------------------------------------
// The command
// ---------------------------------------------------------------------------

pub fn top(args: &[String]) -> io::Result<()> {
    let mut host = None;
    let mut port = PORT;
    let mut once = false;
    let mut args = args.iter();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--once" => once = true,
            "--port" => {
                port = args
                    .next()
                    .and_then(|p| p.parse().ok())
                    .ok_or_else(|| bad("--port needs a port number"))?;
            }
            flag if flag.starts_with('-') => return Err(bad(format!("unknown flag {flag}"))),
            h if host.is_none() => host = Some(h.to_string()),
            extra => return Err(bad(format!("unexpected {extra}"))),
        }
    }
    let config = std::fs::read_to_string(".cargo/config.toml").ok();
    let env = std::env::var("BONSAI_PI").ok();
    let dest = destination(host.as_deref(), env.as_deref(), config.as_deref());
    let name = std::fs::read_to_string("Cargo.toml")
        .ok()
        .and_then(|c| crate::parse_package_name(&c));
    let title = match (&name, &dest) {
        (Some(n), Some(h)) => format!("{n} on {h}"),
        (Some(n), None) => n.clone(),
        (None, Some(h)) => h.clone(),
        (None, None) => format!("127.0.0.1:{port}"),
    };
    if once {
        return print_once(&dest, port, &title);
    }

    let (tx, rx) = mpsc::channel();
    let ssh = Arc::new(Mutex::new(None));
    {
        let (dest, ssh) = (dest.clone(), ssh.clone());
        std::thread::spawn(move || reader(dest, port, tx, ssh));
    }
    let terminal = ratatui::init();
    let result = view::run(terminal, rx, title);
    ratatui::restore();
    if let Some(mut child) = ssh.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = child.kill();
    }
    result
}

/// Snapshots two seconds apart, printed as tables: for scripts, and a
/// terminal that can't show the live view.
fn print_once(dest: &Option<String>, port: u16, title: &str) -> io::Result<()> {
    let (mut r, child) = connect(dest, port)?;
    let first = read_snapshot(&mut r);
    if let Some(mut child) = child
        && matches!(first, Err(_) | Ok(None))
        && let Some(why) = ssh_error(&mut child)
    {
        return Err(bad(why));
    }
    let Some(a) = first? else {
        return Err(bad("the tree stopped"));
    };
    let next =
        |r: &mut Box<dyn BufRead + Send>| read_snapshot(r)?.ok_or_else(|| bad("the tree stopped"));
    let mut b = next(&mut r)?;
    while b.uptime_ms < a.uptime_ms + WINDOW_MS {
        b = next(&mut r)?;
    }
    let ms = b.uptime_ms.saturating_sub(a.uptime_ms);
    println!(
        "{title}: up {}, {:.0} events/s, slowest event {}, {} waiting",
        uptime(b.uptime_ms),
        per_sec(a.events, b.events, ms),
        took(b.max_event_us),
        b.inbox
    );
    println!("{}", record_status(&b).1);
    println!(
        "{:<16} {:>9} {:>9} {:>9} {:>9} {:>7}  status",
        "branch", "inputs/s", "sent/s", "avg µs", "max µs", "panics"
    );
    for (i, br) in b.branches.iter().enumerate() {
        let before = a.branches.get(i).filter(|x| x.name == br.name);
        let (inputs, sent) = before.map_or((0, 0), |x| (x.inputs, x.sent));
        let out = format!("  {}", branch_status(br, &b).1);
        println!(
            "{:<16} {:>9.1} {:>9.1} {:>9} {:>9} {:>7}{out}",
            br.name,
            per_sec(inputs, br.inputs, ms),
            per_sec(sent, br.sent, ms),
            avg_us(br),
            br.max_us,
            br.panics
        );
    }
    if !b.edges.is_empty() {
        println!(
            "{:<16} {:>9} {:>9} {:>9} {:>9} {:>9} {:>7}  status",
            "edge", "state", "in/s", "out/s", "dropped", "lost", "restarts"
        );
        for (i, e) in b.edges.iter().enumerate() {
            let before = a.edges.get(i).filter(|x| x.name == e.name);
            let (rx, tx) = before.map_or((0, 0), |x| (x.received, x.sent));
            println!(
                "{:<16} {:>9} {:>9.1} {:>9.1} {:>9} {:>9} {:>7}  {}",
                e.name,
                e.state,
                per_sec(rx, e.received, ms),
                per_sec(tx, e.sent, ms),
                e.dropped,
                e.lost(),
                e.restarts,
                edge_status(e).1
            );
        }
    }
    if !b.links.is_empty() {
        println!("{:<48} {:>9}", "link", "msgs/s");
        for (i, w) in b.links.iter().enumerate() {
            let before = a.links.get(i).map_or(0, |x| x.count);
            println!("{:<48} {:>9.1}", link_text(w), per_sec(before, w.count, ms));
        }
    }
    Ok(())
}

/// `sensor --Reading--> watchdog, display`, as `bonsai list` shows a link.
pub(crate) fn link_text(w: &Link) -> String {
    let arrow = if w.label.is_empty() {
        "-->".to_string()
    } else {
        format!("--{}-->", w.label)
    };
    format!("{} {arrow} {}", w.from, w.to.join(", "))
}

pub(crate) fn avg_us(b: &Branch) -> u64 {
    // A panicked input isn't timed.
    b.busy_us / (b.inputs - b.panics).max(1)
}

/// Who wrote a log line: `14:05:03.123Z  INFO sensor: 26.5 °C` → `sensor`.
pub fn source(line: &str) -> Option<&str> {
    let rest = line.get(14..)?.trim_start();
    let rest = rest.split_once(' ')?.1;
    Some(rest.split_once(": ")?.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORT: &str = "bonsai-top 1\t1500\t3\t40\t0\n\
         branch\tsensor\t3\t0\t0\t12\t5\t1\t4\n\
         edge\tnet\tretrying\t1\t2\t0\t1\tbind x\t3\t1\t0\n\
         record\tpartial\tlogs/2026-10-01_10-00-00\terrors\tcan't open errors.log: denied\n\
         log\t14:05:03.123Z  INFO sensor: 26.5 °C\n\
         end\n\
         bonsai-top 1\t2000\t4\t40\t0\n\
         edge\told\tup\t5\t6\t0\t0\t\n\
         end\n";

    #[test]
    fn snapshots_read_as_the_runtime_writes_them() {
        let mut r = REPORT.as_bytes();
        let s = read_snapshot(&mut r).unwrap().unwrap();
        assert_eq!(
            (s.uptime_ms, s.events, s.max_event_us, s.inbox),
            (1500, 3, 40, 0)
        );
        assert_eq!(
            s.branches,
            [Branch {
                name: "sensor".into(),
                inputs: 3,
                sent: 0,
                panics: 0,
                busy_us: 12,
                max_us: 5,
                failed: true,
                discarded: 4
            }]
        );
        assert_eq!(s.edges[0].state, "retrying");
        assert_eq!(s.edges[0].error, "bind x");
        assert_eq!(
            (s.edges[0].accepted, s.edges[0].failed, s.edges[0].discarded),
            (3, 1, 0)
        );
        assert_eq!(s.logs, ["14:05:03.123Z  INFO sensor: 26.5 °C"]);
        assert_eq!(
            s.record,
            Some(Record {
                state: "partial".into(),
                detail: "logs/2026-10-01_10-00-00".into(),
                failed: vec![("errors".into(), "can't open errors.log: denied".into())],
            })
        );
        let s = read_snapshot(&mut r).unwrap().unwrap();
        assert_eq!(s.uptime_ms, 2000);
        assert!(s.branches.is_empty());
        // A tree from before record rows: no recorder state.
        assert_eq!(s.record, None);
        // A tree from before the queue counts: they read as 0.
        assert_eq!((s.edges[0].sent, s.edges[0].accepted), (6, 0));
        assert_eq!(read_snapshot(&mut r).unwrap(), None);
    }

    #[test]
    fn a_stranger_or_a_newer_tree_is_refused() {
        let lines = |t: &str| t.lines().map(String::from).collect::<Vec<_>>();
        assert!(parse(&lines("HTTP/1.1 200 OK")).is_err());
        assert!(parse(&lines("bonsai-top 2\t1\t1\t1\t1")).is_err());
        // Rows it doesn't know yet are skipped.
        let s = parse(&lines("bonsai-top 1\t1\t1\t1\t1\nfuture\ta\tb")).unwrap();
        assert_eq!(s.events, 1);
    }

    #[test]
    fn rates_and_uptime() {
        assert_eq!(per_sec(10, 20, 500), 20.0);
        assert_eq!(per_sec(10, 20, 0), 0.0);
        assert_eq!(per_sec(20, 10, 500), 0.0); // a restarted tree
        assert_eq!(uptime(12_400), "12s");
        assert_eq!(uptime(184_000), "3m04s");
        assert_eq!(uptime(3_720_000), "1h02m");
        assert_eq!(took(850), "850 µs");
        assert_eq!(took(12_400), "12.4 ms");
        assert_eq!(took(1_200_000), "1.2 s");
    }

    #[test]
    fn log_lines_name_their_source() {
        assert_eq!(
            source("14:05:03.123Z  INFO sensor: 26.5 °C"),
            Some("sensor")
        );
        assert_eq!(source("14:05:03.123Z ERROR gps: x: y"), Some("gps"));
        assert_eq!(source("short"), None);
    }

    #[test]
    fn top_finds_the_tree() {
        let pi = "[build]\ntarget = \"aarch64-unknown-linux-gnu\"\n\n[env]\nBONSAI_PI = \"pi@orb.local\"\n";
        let host = "# builds for this computer\n[env]\nBONSAI_PI = \"pi@orb.local\"\n";
        assert_eq!(
            destination(None, None, Some(pi)).as_deref(),
            Some("pi@orb.local")
        );
        assert_eq!(destination(None, None, Some(host)), None);
        assert_eq!(destination(None, None, None), None);
        assert_eq!(
            destination(None, Some("me@b"), Some(host)).as_deref(),
            Some("me@b")
        );
        assert_eq!(destination(Some("local"), Some("me@b"), Some(pi)), None);
        assert_eq!(
            destination(Some("x@y"), None, Some(pi)).as_deref(),
            Some("x@y")
        );
    }

    fn branch(name: &str, inputs: u64) -> Branch {
        Branch {
            name: name.into(),
            inputs,
            ..Default::default()
        }
    }

    fn link(from: &str, to: &str, count: u64) -> Link {
        Link {
            from: from.into(),
            label: "Reading".into(),
            to: vec![to.into()],
            count,
        }
    }

    #[test]
    fn a_quiet_branch_says_what_is_known_and_no_more() {
        let newer = Snapshot {
            links: vec![link("sensor", "display", 0)],
            sys: Some(Sys::default()),
            ..Default::default()
        };
        let quiet = |b: &Branch, s: &Snapshot| branch_status(b, s);
        assert_eq!(
            quiet(&branch("display", 3), &newer),
            (Health::Ok, "running".into())
        );
        assert_eq!(
            quiet(&branch("display", 0), &newer),
            (
                Health::Quiet,
                "no input observed; nothing has come over its links yet (from sensor)".into()
            )
        );
        assert_eq!(
            quiet(&branch("sensor", 0), &newer),
            (
                Health::Quiet,
                "no input observed; nothing is linked to it, so only a rate would feed it".into()
            )
        );
        // An older tree doesn't say what feeds what: no more than that.
        assert_eq!(
            quiet(&branch("sensor", 0), &Snapshot::default()),
            (Health::Quiet, "no input observed".into())
        );
        let failed = Branch {
            failed: true,
            discarded: 4,
            ..branch("display", 9)
        };
        let (health, text) = quiet(&failed, &newer);
        assert_eq!(health, Health::Problem);
        assert!(
            text.starts_with("out of service: its setup panicked")
                && text.contains("4 inputs dropped"),
            "{text}"
        );
    }

    #[test]
    fn an_edge_says_whether_its_up_and_what_it_lost() {
        let edge = |state: &str, error: &str| Edge {
            name: "uplink".into(),
            state: state.into(),
            error: error.into(),
            ..Default::default()
        };
        assert_eq!(
            edge_status(&edge("retrying", "bind 0.0.0.0:6969: in use")),
            (
                Health::Problem,
                "retrying: bind 0.0.0.0:6969: in use".into()
            )
        );
        assert_eq!(
            edge_status(&edge("starting", "")),
            (Health::Quiet, "connecting: not up yet".into())
        );
        assert_eq!(
            edge_status(&edge("up", "")),
            (Health::Quiet, "up; no traffic observed".into())
        );
        let busy = Edge {
            received: 5,
            sent: 9,
            ..edge("up", "connect: refused")
        };
        assert_eq!(
            edge_status(&busy),
            (Health::Ok, "up; last error: connect: refused".into())
        );
        let losing = Edge {
            dropped: 3,
            failed: 1,
            discarded: 2,
            ..busy
        };
        assert_eq!(
            edge_status(&losing),
            (
                Health::Problem,
                "up; 3 dropped: its queue was full; 3 lost (1 failed to send, 2 not delivered); last error: connect: refused".into()
            )
        );
    }

    #[test]
    fn the_recorder_says_off_starting_on_or_unavailable() {
        let with = |state: &str, detail: &str| Snapshot {
            record: Some(Record {
                state: state.into(),
                detail: detail.into(),
                failed: Vec::new(),
            }),
            ..Default::default()
        };
        let failing = |state: &str, detail: &str, failed: &[(&str, &str)]| Snapshot {
            record: Some(Record {
                state: state.into(),
                detail: detail.into(),
                failed: failed
                    .iter()
                    .map(|(k, w)| (k.to_string(), w.to_string()))
                    .collect(),
            }),
            ..Default::default()
        };
        // Some files written, some not: never shown as healthy.
        assert_eq!(
            record_status(&failing(
                "partial",
                "logs/r1",
                &[("errors", "can't open errors.log: denied")]
            )),
            (
                Health::Problem,
                "run logs: partly, in logs/r1; not written: errors (can't open errors.log: denied)"
                    .into()
            )
        );
        assert_eq!(
            record_status(&failing(
                "unavailable",
                "nothing can be written in logs/r1",
                &[("events", "can't open events.log: denied"), ("panics", "can't write panics.log: full")]
            )),
            (
                Health::Problem,
                "run logs: unavailable: nothing can be written in logs/r1: events (can't open events.log: denied), panics (can't write panics.log: full)".into()
            )
        );
        assert_eq!(
            record_status(&with("on", "logs/2026-10-01_10-00-00")),
            (
                Health::Ok,
                "run logs: on, in logs/2026-10-01_10-00-00".into()
            )
        );
        assert_eq!(
            record_status(&with("off", "BONSAI_RECORD=off")),
            (Health::Quiet, "run logs: off (BONSAI_RECORD=off)".into())
        );
        assert_eq!(
            record_status(&with("starting", "making a run folder in logs")),
            (
                Health::Quiet,
                "run logs: starting (making a run folder in logs)".into()
            )
        );
        assert_eq!(
            record_status(&with(
                "unavailable",
                "another tree held logs/.bonsai-record.lock for 5s"
            )),
            (
                Health::Problem,
                "run logs: unavailable: another tree held logs/.bonsai-record.lock for 5s".into()
            )
        );
        let (health, text) = record_status(&Snapshot::default());
        assert_eq!(health, Health::Quiet);
        assert!(text.contains("not reported"), "{text}");
    }
}

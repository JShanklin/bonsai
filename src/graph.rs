//! A tree's graph and the code generated from it. Pure: text in, text out.
//!
//! `bonsai.toml` holds the graph: the branches (with their settings and
//! `rate`), the edges (the tree's bridges to the outside world) and the wires
//! between them. `src/messages.rs` holds the message types. From those,
//! `bonsai sync` writes `src/wiring.rs` (the typed inputs, outputs and core),
//! `src/settings.rs`, `src/branches/mod.rs` and `src/edges/mod.rs`.

use std::str::FromStr;

use toml_edit::{DocumentMut, Item, Table, Value};

/// A tree's graph, read from `bonsai.toml`.
#[derive(Debug, Default, PartialEq)]
pub struct Config {
    /// In the file's order, which is the order the core runs them in.
    pub branches: Vec<BranchCfg>,
    pub edges: Vec<EdgeCfg>,
    pub wires: Vec<Wire>,
}

#[derive(Debug, PartialEq)]
pub struct BranchCfg {
    pub name: String,
    /// Ticks per second, when the branch has a `rate`.
    pub rate: Option<f64>,
    /// Every other key in its table, as a typed constant.
    pub settings: Vec<(String, Setting)>,
}

#[derive(Debug, PartialEq)]
pub struct EdgeCfg {
    pub name: String,
    pub kind: EdgeKind,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Framing {
    Raw,
    Lines,
}

/// A built-in edge's settings, or `Custom` (its own code in
/// `src/edges/<name>.rs`, its other keys as settings).
#[derive(Debug, PartialEq)]
pub enum EdgeKind {
    Udp {
        bind: String,
        to: Option<String>,
        join: Vec<String>,
        iface: String,
        reply: bool,
    },
    Tcp {
        connect: Option<String>,
        listen: Option<String>,
        framing: Framing,
    },
    Serial {
        device: String,
        baud: u32,
        framing: Framing,
    },
    Custom {
        settings: Vec<(String, Setting)>,
    },
}

impl EdgeKind {
    /// One line for `bonsai list`.
    pub fn summary(&self) -> String {
        match self {
            EdgeKind::Udp {
                bind,
                to,
                join,
                reply,
                ..
            } => {
                let mut s = format!("udp {bind}");
                if let Some(to) = to {
                    s.push_str(&format!(" → {to}"));
                } else if *reply {
                    s.push_str(" → whoever sent last");
                }
                if !join.is_empty() {
                    s.push_str(&format!(", joins {}", join.join(", ")));
                }
                s
            }
            EdgeKind::Tcp {
                connect, listen, ..
            } => match (connect, listen) {
                (Some(c), _) => format!("tcp client of {c}"),
                (_, Some(l)) => format!("tcp server on {l}"),
                _ => "tcp".to_string(),
            },
            EdgeKind::Serial { device, baud, .. } => format!("serial {device} at {baud}"),
            EdgeKind::Custom { .. } => "custom".to_string(),
        }
    }
}

/// The edge kinds and the keys each takes.
pub const EDGE_KINDS: &[(&str, &[&str])] = &[
    ("udp", &["bind", "to", "join", "iface", "reply"]),
    ("tcp", &["connect", "listen", "framing"]),
    ("serial", &["device", "baud", "framing"]),
    ("custom", &[]),
];

#[derive(Clone, Debug, PartialEq)]
pub enum Setting {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    List(Vec<Setting>),
}

/// `from` sends `message` to every branch in `to`. With no message, one end
/// is an edge: an edge's input to branches, or a branch's output to edges.
#[derive(Clone, Debug, PartialEq)]
pub struct Wire {
    pub from: String,
    pub message: Option<String>,
    pub to: Vec<String>,
}

/// Read `bonsai.toml`. Errors name the key that's wrong.
pub fn parse(src: &str) -> Result<Config, String> {
    let doc = DocumentMut::from_str(src).map_err(|e| format!("bonsai.toml: {e}"))?;
    let mut cfg = Config::default();

    for (name, table) in tables(&doc, "branch")? {
        let mut branch = BranchCfg {
            name: name.to_string(),
            rate: None,
            settings: Vec::new(),
        };
        for (key, item) in table.iter() {
            let value = item
                .as_value()
                .ok_or_else(|| format!("bonsai.toml: [branch.{name}] {key} must be a value"))?;
            if key == "rate" {
                let hz = match value {
                    Value::Integer(i) => *i.value() as f64,
                    Value::Float(f) => *f.value(),
                    _ => {
                        return Err(format!(
                            "bonsai.toml: [branch.{name}] rate must be a number"
                        ));
                    }
                };
                if hz <= 0.0 || !hz.is_finite() {
                    return Err(format!(
                        "bonsai.toml: [branch.{name}] rate must be above 0 (ticks per second)"
                    ));
                }
                branch.rate = Some(hz);
            } else {
                let setting = setting(value)
                    .map_err(|e| format!("bonsai.toml: [branch.{name}] {key}: {e}"))?;
                branch.settings.push((key.to_string(), setting));
            }
        }
        cfg.branches.push(branch);
    }

    for (name, table) in tables(&doc, "edge")? {
        let kind = edge_kind(table).map_err(|e| format!("bonsai.toml: [edge.{name}] {e}"))?;
        cfg.edges.push(EdgeCfg {
            name: name.to_string(),
            kind,
        });
    }

    if let Some(wires) = doc.get("wire") {
        let wires = wires
            .as_array_of_tables()
            .ok_or("bonsai.toml: wires are [[wire]] tables")?;
        for (i, wire) in wires.iter().enumerate() {
            let text = |key: &str| -> Result<Option<String>, String> {
                match wire.get(key) {
                    None => Ok(None),
                    Some(item) => item
                        .as_str()
                        .map(|s| Some(s.to_string()))
                        .ok_or_else(|| format!("bonsai.toml: wire {}: `{key}` is a string", i + 1)),
                }
            };
            let from = text("from")?
                .ok_or_else(|| format!("bonsai.toml: wire {} needs `from = \"…\"`", i + 1))?;
            let message = text("message")?;
            let to = wire
                .get("to")
                .and_then(Item::as_array)
                .ok_or_else(|| format!("bonsai.toml: wire {} needs `to = [\"…\"]`", i + 1))?
                .iter()
                .map(|v| {
                    v.as_str().map(str::to_string).ok_or_else(|| {
                        format!(
                            "bonsai.toml: wire {}: `to` lists branch or edge names",
                            i + 1
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            cfg.wires.push(Wire { from, message, to });
        }
    }
    Ok(cfg)
}

/// The `[<kind>.<name>]` tables, in order.
fn tables<'a>(doc: &'a DocumentMut, kind: &str) -> Result<Vec<(&'a str, &'a Table)>, String> {
    let Some(item) = doc.get(kind) else {
        return Ok(Vec::new());
    };
    let all = item
        .as_table()
        .ok_or_else(|| format!("bonsai.toml: `{kind}` must be tables, like [{kind}.name]"))?;
    all.iter()
        .map(|(name, item)| {
            item.as_table()
                .map(|t| (name, t))
                .ok_or_else(|| format!("bonsai.toml: [{kind}.{name}] must be a table"))
        })
        .collect()
}

fn edge_kind(t: &Table) -> Result<EdgeKind, String> {
    let kind = t
        .get("kind")
        .and_then(Item::as_str)
        .ok_or("needs `kind` (udp, tcp, serial or custom)")?;
    let Some((_, keys)) = EDGE_KINDS.iter().find(|(k, _)| *k == kind) else {
        return Err(format!("kind {kind:?}: use udp, tcp, serial or custom"));
    };
    if kind != "custom" {
        for (key, _) in t.iter() {
            if key != "kind" && !keys.contains(&key) {
                return Err(format!("{key}: a {kind} edge takes {}", keys.join(", ")));
            }
        }
    }
    let text = |key: &str| -> Result<Option<String>, String> {
        match t.get(key) {
            None => Ok(None),
            Some(item) => item
                .as_str()
                .map(|s| Some(s.to_string()))
                .ok_or_else(|| format!("{key} is a string")),
        }
    };
    let needed = |key: &str| -> Result<String, String> {
        text(key)?.ok_or_else(|| format!("a {kind} edge needs `{key}`"))
    };
    let framing = || -> Result<Framing, String> {
        match text("framing")?.as_deref() {
            None | Some("raw") => Ok(Framing::Raw),
            Some("lines") => Ok(Framing::Lines),
            Some(other) => Err(format!("framing {other:?}: use \"raw\" or \"lines\"")),
        }
    };
    Ok(match kind {
        "udp" => {
            let join = match t.get("join") {
                None => Vec::new(),
                Some(item) => match item.as_value() {
                    Some(Value::String(s)) => vec![s.value().clone()],
                    Some(Value::Array(a)) => a
                        .iter()
                        .map(|v| v.as_str().map(str::to_string))
                        .collect::<Option<Vec<_>>>()
                        .ok_or("join is a group address or a list of them")?,
                    _ => return Err("join is a group address or a list of them".to_string()),
                },
            };
            let reply = match t.get("reply") {
                None => false,
                Some(item) => item.as_bool().ok_or("reply is true or false")?,
            };
            EdgeKind::Udp {
                bind: needed("bind")?,
                to: text("to")?,
                join,
                iface: text("iface")?.unwrap_or_else(|| "0.0.0.0".to_string()),
                reply,
            }
        }
        "tcp" => {
            let (connect, listen) = (text("connect")?, text("listen")?);
            if connect.is_some() == listen.is_some() {
                return Err(
                    "a tcp edge takes one of `connect` (a client) or `listen` (a server)"
                        .to_string(),
                );
            }
            EdgeKind::Tcp {
                connect,
                listen,
                framing: framing()?,
            }
        }
        "serial" => {
            let baud = t
                .get("baud")
                .ok_or("a serial edge needs `baud`")?
                .as_integer()
                .and_then(|b| u32::try_from(b).ok())
                .filter(|b| *b > 0)
                .ok_or("baud is a positive number")?;
            EdgeKind::Serial {
                device: needed("device")?,
                baud,
                framing: framing()?,
            }
        }
        _ => {
            let mut settings = Vec::new();
            for (key, item) in t.iter().filter(|(k, _)| *k != "kind") {
                let value = item
                    .as_value()
                    .ok_or_else(|| format!("{key} must be a value"))?;
                settings.push((
                    key.to_string(),
                    setting(value).map_err(|e| format!("{key}: {e}"))?,
                ));
            }
            EdgeKind::Custom { settings }
        }
    })
}

fn setting(value: &Value) -> Result<Setting, String> {
    Ok(match value {
        Value::String(s) => Setting::Str(s.value().clone()),
        Value::Integer(i) => Setting::Int(*i.value()),
        Value::Float(f) => Setting::Float(*f.value()),
        Value::Boolean(b) => Setting::Bool(*b.value()),
        Value::Array(items) => {
            let items = items.iter().map(setting).collect::<Result<Vec<_>, _>>()?;
            let Some(first) = items.first() else {
                return Err("an empty list has no type; give it a value".to_string());
            };
            if items
                .iter()
                .any(|i| std::mem::discriminant(i) != std::mem::discriminant(first))
            {
                return Err("a list's values must all have one type".to_string());
            }
            if matches!(first, Setting::List(_)) {
                return Err("lists of lists aren't supported".to_string());
            }
            Setting::List(items)
        }
        Value::Datetime(_) | Value::InlineTable(_) => {
            return Err("use a string, number, bool or list".to_string());
        }
    })
}

/// The message types declared in `src/messages.rs`: every top-level
/// `pub struct Name` (unit, tuple or with fields).
pub fn parse_messages(src: &str) -> Vec<String> {
    src.lines()
        .filter(|l| !l.starts_with(char::is_whitespace))
        .filter_map(|l| l.strip_prefix("pub struct "))
        .map(|rest| {
            rest.chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect::<String>()
        })
        .filter(|name| !name.is_empty())
        .collect()
}

/// `radio_link` → `RadioLink`: a branch's struct name, an edge's input variant.
pub fn camel(snake: &str) -> String {
    snake
        .split('_')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(c) => c.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "gen", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut",
    "pub", "ref", "return", "self", "static", "struct", "super", "trait", "true", "type", "unsafe",
    "use", "where", "while", "yield", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "try", "typeof", "unsized", "virtual",
];

/// Names the generated code keeps for itself.
pub const RESERVED_NAMES: &[&str] = &["serial"];

/// A usable branch or edge name: snake_case, and not a Rust keyword.
pub fn is_branch_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !KEYWORDS.contains(&name)
}

/// A usable setting key: becomes an UPPER_CASE constant.
fn is_setting_key(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// What `sync` found wrong (refuses to generate) and what's worth a look.
#[derive(Debug, Default, PartialEq)]
pub struct Report {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl Config {
    pub fn is_branch(&self, name: &str) -> bool {
        self.branches.iter().any(|b| b.name == name)
    }

    pub fn edge(&self, name: &str) -> Option<&EdgeCfg> {
        self.edges.iter().find(|e| e.name == name)
    }

    pub fn has_serial(&self) -> bool {
        self.edges
            .iter()
            .any(|e| matches!(e.kind, EdgeKind::Serial { .. }))
    }
}

/// How a wire reads: `a --Msg--> b, c` or `tak --> atak`.
pub fn describe(w: &Wire) -> String {
    match &w.message {
        Some(m) => format!("{} --{m}--> {}", w.from, w.to.join(", ")),
        None => format!("{} --> {}", w.from, w.to.join(", ")),
    }
}

/// Check the graph against itself and the declared messages.
pub fn check(cfg: &Config, messages: &[String]) -> Report {
    let mut r = Report::default();

    let mut names: Vec<&str> = Vec::new();
    let nodes = cfg
        .branches
        .iter()
        .map(|b| (&b.name, "branch"))
        .chain(cfg.edges.iter().map(|e| (&e.name, "edge")));
    for (name, what) in nodes {
        if !is_branch_name(name) {
            r.errors.push(format!(
                "{what} `{name}`: a name is snake_case (a-z, 0-9, _) and not a Rust keyword"
            ));
        }
        if RESERVED_NAMES.contains(&name.as_str()) {
            r.errors.push(format!(
                "{what} `{name}`: bonsai uses that name; pick another"
            ));
        }
        if names.iter().any(|n| camel(n) == camel(name)) {
            r.errors.push(format!(
                "{what} `{name}` shares its name ({}) with another branch or edge",
                camel(name)
            ));
        }
        names.push(name);
    }
    for b in &cfg.branches {
        for (key, _) in &b.settings {
            if !is_setting_key(key) {
                r.errors.push(format!(
                    "[branch.{}] {key}: a setting name is letters, digits and _",
                    b.name
                ));
            }
        }
    }
    for e in &cfg.edges {
        if messages.contains(&camel(&e.name)) || camel(&e.name) == "Tick" {
            r.errors.push(format!(
                "edge `{}`: its input is called {}, like a message; rename one",
                e.name,
                camel(&e.name)
            ));
        }
        if let EdgeKind::Custom { settings } = &e.kind {
            for (key, _) in settings {
                if !is_setting_key(key) {
                    r.errors.push(format!(
                        "[edge.{}] {key}: a setting name is letters, digits and _",
                        e.name
                    ));
                }
            }
        }
    }

    for (i, w) in cfg.wires.iter().enumerate() {
        let wire = format!("wire {}", describe(w));
        let from_edge = cfg.edge(&w.from).is_some();
        if !cfg.is_branch(&w.from) && !from_edge {
            r.errors
                .push(format!("{wire}: no branch or edge `{}`", w.from));
        }
        match &w.message {
            Some(m) => {
                if !messages.contains(m) {
                    r.errors
                        .push(format!("{wire}: no message `{m}` in src/messages.rs"));
                }
                if m == "Tick" {
                    r.errors.push(format!(
                        "{wire}: `Tick` is the input a branch's rate makes; rename the message"
                    ));
                }
                if from_edge {
                    r.errors.push(format!(
                        "{wire}: an edge sends what it receives, not a message; drop `message`"
                    ));
                }
                for to in &w.to {
                    if cfg.edge(to).is_some() {
                        r.errors.push(format!(
                            "{wire}: to send to edge `{to}`, wire without a message (out.to_{to}(..))"
                        ));
                    }
                }
            }
            None if from_edge => {
                for to in &w.to {
                    if cfg.edge(to).is_some() {
                        r.errors.push(format!(
                            "{wire}: edges don't wire to edges; put a branch between them"
                        ));
                    }
                }
            }
            None => {
                for to in &w.to {
                    if cfg.is_branch(to) {
                        r.errors.push(format!(
                            "{wire}: a wire between branches carries a message; add `message`"
                        ));
                    }
                }
            }
        }
        if w.to.is_empty() {
            r.errors.push(format!("{wire}: `to` is empty"));
        }
        for (j, to) in w.to.iter().enumerate() {
            if !cfg.is_branch(to) && cfg.edge(to).is_none() {
                r.errors.push(format!("{wire}: no branch or edge `{to}`"));
            }
            if *to == w.from {
                r.errors.push(format!(
                    "{wire}: a branch can't send to itself; keep that state in the branch"
                ));
            }
            if w.to[..j].contains(to) {
                r.errors.push(format!("{wire}: `{to}` is listed twice"));
            }
        }
        if cfg.wires[..i]
            .iter()
            .any(|o| o.from == w.from && o.message == w.message)
        {
            r.errors.push(format!(
                "{wire}: wired twice; put every receiver in one `to` list"
            ));
        }
    }

    for b in &cfg.branches {
        let fed = cfg.wires.iter().any(|w| w.to.contains(&b.name));
        if !fed && b.rate.is_none() {
            r.warnings.push(format!(
                "{} has no inputs, so its process never runs: wire something to it, or give it a rate",
                b.name
            ));
        }
    }
    for e in &cfg.edges {
        if !cfg
            .wires
            .iter()
            .any(|w| w.from == e.name || w.to.contains(&e.name))
        {
            r.warnings.push(format!(
                "edge {} isn't wired: `bonsai wire {} <branch>` to hear it, `bonsai wire <branch> {}` to send",
                e.name, e.name, e.name
            ));
        }
    }
    for cycle in cycles(cfg) {
        r.warnings.push(format!(
            "{} send to each other in a loop: make at least one of those sends conditional",
            cycle.join(" → ")
        ));
    }
    r
}

/// Every loop of branches sending to each other, once each, as the branch
/// names in order (the first repeated at the end).
fn cycles(cfg: &Config) -> Vec<Vec<String>> {
    let names: Vec<&str> = cfg.branches.iter().map(|b| b.name.as_str()).collect();
    let next = |from: &str| -> Vec<&str> {
        let mut to: Vec<&str> = cfg
            .wires
            .iter()
            .filter(|w| w.from == from)
            .flat_map(|w| w.to.iter().map(String::as_str))
            .filter(|t| names.contains(t))
            .collect();
        to.dedup();
        to
    };
    let mut found: Vec<Vec<String>> = Vec::new();
    // Search from each branch for a path back to it through later branches
    // only, so every loop is found once, from its first branch.
    for (start_i, &start) in names.iter().enumerate() {
        let mut stack: Vec<(Vec<&str>, Vec<&str>)> = vec![(vec![start], next(start))];
        while let Some((path, mut todo)) = stack.pop() {
            let Some(n) = todo.pop() else { continue };
            stack.push((path.clone(), todo));
            if n == start {
                let mut cycle: Vec<String> = path.iter().map(|s| s.to_string()).collect();
                cycle.push(start.to_string());
                found.push(cycle);
            } else if !path.contains(&n)
                && names
                    .iter()
                    .position(|x| *x == n)
                    .is_some_and(|i| i > start_i)
            {
                let mut p = path.clone();
                p.push(n);
                stack.push((p, next(n)));
            }
        }
    }
    found
}

/// A branch's input variants, in order: `Tick` with a rate, then each
/// message or edge wired to it (by the edge's CamelCase name), without
/// repeats. The arms its `match input` needs.
pub fn input_variants(cfg: &Config, branch: &str) -> Vec<String> {
    let mut variants: Vec<String> = Vec::new();
    if cfg
        .branches
        .iter()
        .any(|b| b.name == branch && b.rate.is_some())
    {
        variants.push("Tick".to_string());
    }
    for w in cfg
        .wires
        .iter()
        .filter(|w| w.to.iter().any(|t| t == branch))
    {
        let v = match &w.message {
            Some(m) => m.clone(),
            None => camel(&w.from),
        };
        if !variants.contains(&v) {
            variants.push(v);
        }
    }
    variants
}

/// The Rust type of what an edge hands the core, and takes from it.
fn edge_types(e: &EdgeCfg) -> (String, String) {
    match e.kind {
        EdgeKind::Custom { .. } => {
            let t = format!("crate::edges::{}::{}", e.name, camel(&e.name));
            (
                format!("<{t} as crate::bonsai::Edge>::In"),
                format!("<{t} as crate::bonsai::Edge>::Out"),
            )
        }
        _ => (
            "crate::bonsai::Packet".to_string(),
            "crate::bonsai::Packet".to_string(),
        ),
    }
}

/// A wire's variant in `Msg`: `SensorReading` for sensor sending Reading,
/// `AtakToTak` for atak sending to the tak edge (one per edge it feeds).
fn msg_variant(from: &str, message: &str) -> String {
    format!("{}{}", camel(from), message)
}

fn edge_msg_variant(from: &str, edge: &str) -> String {
    format!("{}To{}", camel(from), camel(edge))
}

const HEADER: &str = "//!\n\
//! Generated by `bonsai sync` from bonsai.toml and src/messages.rs. Don't edit\n\
//! it: change those, then run `bonsai sync` (every bonsai command that changes\n\
//! the graph runs it for you).\n";

/// `src/wiring.rs`: each branch's `Input` and `Out`, the edges, and the core
/// that delivers every message. Assumes `check` found no errors.
pub fn render_wiring(cfg: &Config) -> String {
    let mut o = format!("//! The wiring between branches and edges.\n{HEADER}");
    o.push_str(
        "#![allow(dead_code, unused_imports, unused_variables)]
#![allow(clippy::enum_variant_names, clippy::wrong_self_convention)]

use std::collections::VecDeque;

use tokio::sync::mpsc;

use crate::bonsai::{EdgeOut, Event, Sends, Slot, Tree, drain, spawn_edge};
use crate::branches;
use crate::messages::*;

/// What each edge hands the core.
#[derive(Debug)]
pub enum EdgeIn {
",
    );
    for e in &cfg.edges {
        o.push_str(&format!("    {}({}),\n", camel(&e.name), edge_types(e).0));
    }
    o.push_str("}\n\n/// Every message in flight: one variant per wire.\n#[derive(Debug)]\npub enum Msg {\n");
    for w in &cfg.wires {
        match &w.message {
            Some(m) => o.push_str(&format!(
                "    /// {}\n    {}({m}),\n",
                describe(w),
                msg_variant(&w.from, m)
            )),
            None if cfg.is_branch(&w.from) => {
                for to in &w.to {
                    if let Some(e) = cfg.edge(to) {
                        o.push_str(&format!(
                            "    /// {} --> {to}\n    {}({}),\n",
                            w.from,
                            edge_msg_variant(&w.from, to),
                            edge_types(e).1
                        ));
                    }
                }
            }
            None => {}
        }
    }
    o.push_str("}\n");

    for b in &cfg.branches {
        o.push_str(&format!(
            "
pub mod {name} {{
    use super::*;

    /// What {name} receives.
    #[derive(Debug)]
    pub enum Input {{
",
            name = b.name
        ));
        for v in input_variants(cfg, &b.name) {
            if v == "Tick" {
                o.push_str("        /// Its `rate` ticked.\n        Tick,\n");
            } else if let Some(e) = cfg.edges.iter().find(|e| camel(&e.name) == v) {
                o.push_str(&format!(
                    "        /// From the {} edge.\n        {v}({}),\n",
                    e.name,
                    edge_types(e).0
                ));
            } else {
                o.push_str(&format!("        {v}({v}),\n"));
            }
        }
        o.push_str(&format!(
            "    }}

    /// Where {name} sends: `out.send(message)`, `out.to_<edge>(..)`.
    #[derive(Debug, Default)]
    pub struct Out {{
        pub(super) sent: Vec<Msg>,
    }}

    impl Out {{
        /// Send `message` down its wire. Only what {name} is wired to send
        /// compiles.
        pub fn send<M>(&mut self, message: M)
        where
            Self: Sends<M>,
        {{
            Sends::send(self, message);
        }}
",
            name = b.name
        ));
        for w in cfg
            .wires
            .iter()
            .filter(|w| w.from == b.name && w.message.is_none())
        {
            for to in &w.to {
                if let Some(e) = cfg.edge(to) {
                    o.push_str(&format!(
                        "
        /// Hand the {to} edge something to carry out.
        pub fn to_{to}(&mut self, value: {t}) {{
            self.sent.push(Msg::{v}(value));
        }}
",
                        t = edge_types(e).1,
                        v = edge_msg_variant(&b.name, to)
                    ));
                }
            }
        }
        o.push_str(
            "
        /// Everything sent so far, oldest first: for tests.
        pub fn sent(&self) -> &[Msg] {
            &self.sent
        }
    }

    impl crate::bonsai::Outbox for Out {
        fn count(&self) -> usize {
            self.sent.len()
        }
    }
",
        );
        for w in cfg.wires.iter().filter(|w| w.from == b.name) {
            if let Some(m) = &w.message {
                o.push_str(&format!(
                    "
    /// To {to}.
    impl Sends<{m}> for Out {{
        fn send(&mut self, message: {m}) {{
            self.sent.push(Msg::{v}(message));
        }}
    }}
",
                    to = w.to.join(", "),
                    v = msg_variant(&b.name, m)
                ));
            }
        }
        o.push_str("}\n");
    }

    // The built-in edges' settings.
    let mut configs = String::new();
    for e in &cfg.edges {
        let lit = |s: &str| format!("{s:?}");
        let opt = |s: &Option<String>| match s {
            Some(s) => format!("Some({s:?})"),
            None => "None".to_string(),
        };
        let framing = |f: &Framing| match f {
            Framing::Raw => "crate::bonsai::Framing::Raw",
            Framing::Lines => "crate::bonsai::Framing::Lines",
        };
        let upper = e.name.to_ascii_uppercase();
        match &e.kind {
            EdgeKind::Udp {
                bind,
                to,
                join,
                iface,
                reply,
            } => configs.push_str(&format!(
                "const {upper}: crate::bonsai::UdpConfig = crate::bonsai::UdpConfig {{
    bind: {},
    to: {},
    join: &[{}],
    iface: {},
    reply: {reply},
}};
",
                lit(bind),
                opt(to),
                join.iter().map(|g| lit(g)).collect::<Vec<_>>().join(", "),
                lit(iface)
            )),
            EdgeKind::Tcp {
                connect,
                listen,
                framing: f,
            } => configs.push_str(&format!(
                "const {upper}: crate::bonsai::TcpConfig = crate::bonsai::TcpConfig {{
    connect: {},
    listen: {},
    framing: {},
}};
",
                opt(connect),
                opt(listen),
                framing(f)
            )),
            EdgeKind::Serial {
                device,
                baud,
                framing: f,
            } => configs.push_str(&format!(
                "const {upper}: crate::edges::serial::SerialConfig = crate::edges::serial::SerialConfig {{
    device: {},
    baud: {baud},
    framing: {},
}};
",
                lit(device),
                framing(f)
            )),
            EdgeKind::Custom { .. } => {}
        }
    }
    if !configs.is_empty() {
        o.push_str("\n// The edges' settings, from their [edge.<name>] tables.\n");
        o.push_str(&configs);
    }

    o.push_str("\n/// Every branch and edge, set up and waiting for events.\npub struct Core {\n");
    for b in &cfg.branches {
        o.push_str(&format!(
            "    {}: Slot<branches::{}::{}>,\n",
            b.name,
            b.name,
            camel(&b.name)
        ));
    }
    for e in &cfg.edges {
        o.push_str(&format!("    {}: EdgeOut<{}>,\n", e.name, edge_types(e).1));
    }
    o.push_str(
        "    queue: VecDeque<Msg>,\n}\n\nimpl Core {\n    pub fn new() -> Self {\n        Core {\n",
    );
    for b in &cfg.branches {
        o.push_str(&format!(
            "            {n}: Slot::new(\"{n}\"),\n",
            n = b.name
        ));
    }
    for e in &cfg.edges {
        o.push_str(&format!(
            "            {n}: EdgeOut::new(\"{n}\"),\n",
            n = e.name
        ));
    }
    o.push_str("            queue: VecDeque::new(),\n        }\n    }\n");
    for e in &cfg.edges {
        o.push_str(&format!(
            "
    /// What branches sent the {n} edge while it wasn't running: in tests
    /// (edges start only in `bonsai::run`), everything they sent it.
    pub fn drain_{n}(&mut self) -> Vec<{t}> {{
        self.{n}.drain()
    }}
",
            n = e.name,
            t = edge_types(e).1
        ));
    }
    o.push_str(
        "
    /// Hand one message to every branch or edge wired to it, in
    /// bonsai.toml's order.
    fn deliver(&mut self, message: Msg, queue: &mut VecDeque<Msg>) {
        match message {
",
    );
    for w in &cfg.wires {
        match &w.message {
            Some(m) => {
                o.push_str(&format!(
                    "            Msg::{}(message) => {{\n",
                    msg_variant(&w.from, m)
                ));
                deliveries(&mut o, &w.to, m, "message");
                o.push_str("            }\n");
            }
            None if cfg.is_branch(&w.from) => {
                for to in &w.to {
                    o.push_str(&format!(
                        "            Msg::{}(value) => self.{to}.send(value),\n",
                        edge_msg_variant(&w.from, to)
                    ));
                }
            }
            None => {}
        }
    }
    o.push_str(
        "        }
    }
}

impl Default for Core {
    fn default() -> Self {
        Self::new()
    }
}

impl Tree for Core {
    type EdgeIn = EdgeIn;

    fn rates(&self) -> Vec<(usize, f64)> {
        vec![",
    );
    let rated: Vec<String> = cfg
        .branches
        .iter()
        .enumerate()
        .filter_map(|(i, b)| b.rate.map(|hz| format!("({i}, {hz:?})")))
        .collect();
    o.push_str(&rated.join(", "));
    o.push_str(
        "]\n    }\n\n    fn start_edges(&mut self, events: &mpsc::Sender<Event<EdgeIn>>) {\n",
    );
    for e in &cfg.edges {
        let setup = match &e.kind {
            EdgeKind::Udp { .. } => format!(
                "|| crate::bonsai::Udp::setup({})",
                e.name.to_ascii_uppercase()
            ),
            EdgeKind::Tcp { .. } => format!(
                "|| crate::bonsai::Tcp::setup({})",
                e.name.to_ascii_uppercase()
            ),
            EdgeKind::Serial { .. } => format!(
                "|| crate::edges::serial::Serial::setup({})",
                e.name.to_ascii_uppercase()
            ),
            EdgeKind::Custom { .. } => {
                format!("crate::edges::{}::{}::setup", e.name, camel(&e.name))
            }
        };
        o.push_str(&format!(
            "        let tx = spawn_edge(\"{n}\", {setup}, events.clone(), EdgeIn::{c});\n        self.{n}.connect(tx);\n",
            n = e.name,
            c = camel(&e.name)
        ));
    }
    if cfg.edges.is_empty() {
        o.push_str("        let _ = events;\n");
    }
    o.push_str(
        "    }

    fn handle(&mut self, event: Event<EdgeIn>) {
        let mut queue = std::mem::take(&mut self.queue);
        match event {
",
    );
    for (i, b) in cfg.branches.iter().enumerate() {
        if b.rate.is_some() {
            o.push_str(&format!(
                "            Event::Tick({i}) => queue.extend(self.{n}.process({n}::Input::Tick).sent),\n",
                n = b.name
            ));
        }
    }
    o.push_str("            Event::Tick(_) => {}\n");
    for e in &cfg.edges {
        let c = camel(&e.name);
        let to: Vec<String> = cfg
            .wires
            .iter()
            .filter(|w| w.from == e.name)
            .flat_map(|w| w.to.clone())
            .collect();
        if to.is_empty() {
            o.push_str(&format!(
                "            Event::Edge(EdgeIn::{c}(_)) => {{}}\n"
            ));
        } else {
            o.push_str(&format!(
                "            Event::Edge(EdgeIn::{c}(value)) => {{\n"
            ));
            deliveries(&mut o, &to, &c, "value");
            o.push_str("            }\n");
        }
    }
    o.push_str(
        "        }
        drain(&mut queue, |message, queue| self.deliver(message, queue));
        self.queue = queue;
    }
}
",
    );
    o
}

/// Hand `value` to each branch in `to` as `Input::<variant>`, cloning for
/// all but the last.
fn deliveries(o: &mut String, to: &[String], variant: &str, value: &str) {
    for (i, t) in to.iter().enumerate() {
        let v = if i + 1 == to.len() {
            value.to_string()
        } else {
            format!("{value}.clone()")
        };
        o.push_str(&format!(
            "                queue.extend(self.{t}.process({t}::Input::{variant}({v})).sent);\n"
        ));
    }
}

/// `src/settings.rs`: each branch's (and custom edge's) values as constants.
pub fn render_settings(cfg: &Config) -> String {
    let mut o = format!(
        "//! Each branch's settings, from its [branch.<name>] table (and each custom\n\
         //! edge's, from its [edge.<name>]).\n{HEADER}\
         #![allow(dead_code)]\n"
    );
    let custom = cfg.edges.iter().filter_map(|e| match &e.kind {
        EdgeKind::Custom { settings } => Some((&e.name, settings)),
        _ => None,
    });
    for (name, settings) in cfg
        .branches
        .iter()
        .map(|b| (&b.name, &b.settings))
        .chain(custom)
        .filter(|(_, s)| !s.is_empty())
    {
        o.push_str(&format!("\npub mod {name} {{\n"));
        for (key, value) in settings {
            o.push_str(&format!(
                "    pub const {}: {} = {};\n",
                key.to_ascii_uppercase(),
                rust_type(value),
                rust_value(value)
            ));
        }
        o.push_str("}\n");
    }
    o
}

fn rust_type(s: &Setting) -> String {
    match s {
        Setting::Str(_) => "&str".to_string(),
        Setting::Int(_) => "i64".to_string(),
        Setting::Float(_) => "f64".to_string(),
        Setting::Bool(_) => "bool".to_string(),
        Setting::List(items) => format!("&[{}]", rust_type(&items[0])),
    }
}

fn rust_value(s: &Setting) -> String {
    match s {
        Setting::Str(v) => format!("{v:?}"),
        Setting::Int(v) => v.to_string(),
        Setting::Float(v) => format!("{v:?}"),
        Setting::Bool(v) => v.to_string(),
        Setting::List(items) => format!(
            "&[{}]",
            items.iter().map(rust_value).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// `src/branches/mod.rs`: one module per branch.
pub fn render_mod(cfg: &Config) -> String {
    let names = cfg.branches.iter().map(|b| (b.name.as_str(), "")).collect();
    mods("//! The branches.\n", names)
}

/// `src/edges/mod.rs`: one module per custom edge, plus bonsai's serial edge
/// while the tree has one.
pub fn render_edges_mod(cfg: &Config) -> String {
    let mut names: Vec<(&str, &str)> = cfg
        .edges
        .iter()
        .filter(|e| matches!(e.kind, EdgeKind::Custom { .. }))
        .map(|e| (e.name.as_str(), ""))
        .collect();
    if cfg.has_serial() {
        names.push(("serial", " // bonsai's serial edge"));
    }
    mods("//! The custom edges.\n", names)
}

/// A `mod.rs` of `pub mod`s, sorted as `cargo fmt` sorts them (so it leaves
/// the file alone).
fn mods(title: &str, mut names: Vec<(&str, &str)>) -> String {
    names.sort();
    let mut o = format!("{title}{HEADER}");
    if !names.is_empty() {
        o.push('\n');
    }
    for (name, comment) in names {
        o.push_str(&format!("pub mod {name};{comment}\n"));
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    const TREE: &str = r#"
[tree]

[branch.pulse]
rate = 2

[branch.radio]
to = "10.0.0.2:6970"
port = 6969
gain = 1.5
live = true
groups = ["239.2.3.1", "239.2.3.2"]

[branch.log]

[[wire]]
from = "pulse"
message = "Beat"
to = ["radio", "log"]

[[wire]]
from = "radio"
message = "Frame"
to = ["log"]
"#;

    fn messages() -> Vec<String> {
        vec!["Beat".to_string(), "Frame".to_string()]
    }

    #[test]
    fn parses_branches_settings_and_wires() {
        let cfg = parse(TREE).unwrap();
        let names: Vec<&str> = cfg.branches.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["pulse", "radio", "log"]);
        assert_eq!(cfg.branches[0].rate, Some(2.0));
        assert_eq!(
            cfg.branches[1].settings,
            [
                ("to".to_string(), Setting::Str("10.0.0.2:6970".to_string())),
                ("port".to_string(), Setting::Int(6969)),
                ("gain".to_string(), Setting::Float(1.5)),
                ("live".to_string(), Setting::Bool(true)),
                (
                    "groups".to_string(),
                    Setting::List(vec![
                        Setting::Str("239.2.3.1".to_string()),
                        Setting::Str("239.2.3.2".to_string())
                    ])
                ),
            ]
        );
        assert_eq!(
            cfg.wires[0],
            Wire {
                from: "pulse".to_string(),
                message: Some("Beat".to_string()),
                to: vec!["radio".to_string(), "log".to_string()],
            }
        );
        assert_eq!(check(&cfg, &messages()), Report::default());
    }

    #[test]
    fn bad_values_are_refused_with_their_key() {
        let err = parse("[branch.a]\nrate = 0\n").unwrap_err();
        assert!(err.contains("[branch.a] rate"), "{err}");
        let err = parse("[branch.a]\nx = [1, \"b\"]\n").unwrap_err();
        assert!(err.contains("[branch.a] x"), "{err}");
        let err = parse("[[wire]]\nmessage = \"M\"\nto = [\"b\"]\n").unwrap_err();
        assert!(err.contains("from"), "{err}");
    }

    #[test]
    fn check_finds_what_would_not_compile() {
        let cfg = parse(
            r#"
[branch.a]
[branch.b]
[branch.Bad]
[[wire]]
from = "a"
message = "Nope"
to = ["b", "b", "a", "ghost"]
[[wire]]
from = "a"
message = "Nope"
to = ["b"]
"#,
        )
        .unwrap();
        let r = check(&cfg, &messages());
        let all = r.errors.join("\n");
        for expected in [
            "branch `Bad`",
            "no message `Nope`",
            "`b` is listed twice",
            "can't send to itself",
            "no branch or edge `ghost`",
            "wired twice",
        ] {
            assert!(all.contains(expected), "missing {expected:?} in:\n{all}");
        }
    }

    #[test]
    fn warns_about_loops_and_branches_that_never_run() {
        let cfg = parse(
            r#"
[branch.gcs]
rate = 1
[branch.atak]
[branch.idle]
[[wire]]
from = "gcs"
message = "Beat"
to = ["atak"]
[[wire]]
from = "atak"
message = "Frame"
to = ["gcs"]
"#,
        )
        .unwrap();
        let r = check(&cfg, &messages());
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(r.warnings.len(), 2, "{:?}", r.warnings);
        assert!(r.warnings[0].starts_with("idle has no inputs"));
        assert!(r.warnings[1].starts_with("gcs → atak → gcs"));
    }

    #[test]
    fn messages_are_the_top_level_pub_structs() {
        let src = "//! doc\n\n#[derive(Clone, Debug)]\npub struct Beat;\n\
                   pub struct Reading {\n    pub struct_like: u8,\n}\npub struct Pair(u8, u8);\n\
                   struct Private;\n// bonsai:message\n";
        assert_eq!(parse_messages(src), ["Beat", "Reading", "Pair"]);
    }

    #[test]
    fn names() {
        assert_eq!(camel("radio_link"), "RadioLink");
        assert_eq!(camel("gcs2"), "Gcs2");
        assert!(is_branch_name("radio_link"));
        assert!(!is_branch_name("Radio") && !is_branch_name("match") && !is_branch_name("2x"));
    }

    #[test]
    fn wiring_types_each_branch_and_delivers_in_order() {
        let w = render_wiring(&parse(TREE).unwrap());
        // Inputs: the rate's Tick, then each wired message.
        assert!(w.contains("pub mod pulse {"), "{w}");
        assert!(w.contains("        Tick,\n    }"), "{w}");
        assert!(w.contains("pub mod log {"), "{w}");
        assert!(
            w.contains("        Beat(Beat),\n        Frame(Frame),\n    }"),
            "{w}"
        );
        // A branch can send only what it's wired to send.
        assert!(w.contains("impl Sends<Beat> for Out"), "{w}");
        assert!(
            w.contains("self.sent.push(Msg::PulseBeat(message));"),
            "{w}"
        );
        assert_eq!(w.matches("impl Sends<").count(), 2, "{w}");
        // Fan-out clones for all but the last receiver, in bonsai.toml's order.
        assert!(w.contains(
            "                queue.extend(self.radio.process(radio::Input::Beat(message.clone())).sent);\n\
             \x20               queue.extend(self.log.process(log::Input::Beat(message)).sent);\n"
        ), "{w}");
        assert!(w.contains("vec![(0, 2.0)]"), "{w}");
        assert!(
            w.contains(
                "Event::Tick(0) => queue.extend(self.pulse.process(pulse::Input::Tick).sent),"
            ),
            "{w}"
        );
        assert!(
            w.contains(
                "    pulse: Slot<branches::pulse::Pulse>,\n    radio: Slot<branches::radio::Radio>,"
            ),
            "{w}"
        );
    }

    #[test]
    fn an_empty_tree_still_renders() {
        let w = render_wiring(&Config::default());
        assert!(w.contains("pub enum Msg {\n}\n"), "{w}");
        assert!(w.contains("match message {\n        }"), "{w}");
        assert!(w.contains("vec![]"), "{w}");
    }

    #[test]
    fn settings_become_typed_constants() {
        let s = render_settings(&parse(TREE).unwrap());
        assert!(s.contains("pub mod radio {\n"), "{s}");
        assert!(
            s.contains("    pub const TO: &str = \"10.0.0.2:6970\";\n"),
            "{s}"
        );
        assert!(s.contains("    pub const PORT: i64 = 6969;\n"), "{s}");
        assert!(s.contains("    pub const GAIN: f64 = 1.5;\n"), "{s}");
        assert!(s.contains("    pub const LIVE: bool = true;\n"), "{s}");
        assert!(
            s.contains("    pub const GROUPS: &[&str] = &[\"239.2.3.1\", \"239.2.3.2\"];\n"),
            "{s}"
        );
        // No settings, no module.
        assert!(!s.contains("pub mod pulse"), "{s}");
    }

    #[test]
    fn mod_lists_every_branch() {
        let m = render_mod(&parse(TREE).unwrap());
        assert!(
            m.ends_with("\npub mod log;\npub mod pulse;\npub mod radio;\n"),
            "{m}"
        );
    }

    const EDGES: &str = r#"
[branch.atak]
[branch.gcs]
rate = 1

[edge.tak]
kind = "udp"
bind = "0.0.0.0:6969"
to = "100.125.26.5:6970"
join = "239.2.3.2"

[edge.fc]
kind = "udp"
bind = "0.0.0.0:14560"
reply = true

[edge.gps]
kind = "serial"
device = "/dev/serial0"
baud = 921600
framing = "lines"

[edge.link]
kind = "tcp"
connect = "10.0.0.2:5760"

[edge.radio]
kind = "custom"
channel = 7

[[wire]]
from = "tak"
to = ["atak"]

[[wire]]
from = "fc"
to = ["gcs", "atak"]

[[wire]]
from = "atak"
to = ["tak", "fc"]

[[wire]]
from = "gcs"
message = "Beat"
to = ["atak"]

[[wire]]
from = "gps"
to = ["gcs"]

[[wire]]
from = "gcs"
to = ["link", "radio"]
"#;

    #[test]
    fn edges_parse_with_their_kinds() {
        let cfg = parse(EDGES).unwrap();
        let kinds: Vec<&str> = cfg.edges.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(kinds, ["tak", "fc", "gps", "link", "radio"]);
        assert_eq!(
            cfg.edges[0].kind,
            EdgeKind::Udp {
                bind: "0.0.0.0:6969".to_string(),
                to: Some("100.125.26.5:6970".to_string()),
                join: vec!["239.2.3.2".to_string()],
                iface: "0.0.0.0".to_string(),
                reply: false,
            }
        );
        assert_eq!(
            cfg.edges[2].kind,
            EdgeKind::Serial {
                device: "/dev/serial0".to_string(),
                baud: 921_600,
                framing: Framing::Lines,
            }
        );
        assert_eq!(
            cfg.edges[4].kind,
            EdgeKind::Custom {
                settings: vec![("channel".to_string(), Setting::Int(7))]
            }
        );
        assert_eq!(cfg.wires[0].message, None);
        assert_eq!(check(&cfg, &messages()), Report::default());
        assert_eq!(input_variants(&cfg, "atak"), ["Tak", "Fc", "Beat"]);
        assert_eq!(input_variants(&cfg, "gcs"), ["Tick", "Fc", "Gps"]);
    }

    #[test]
    fn edge_tables_are_checked_when_read() {
        for (table, expect) in [
            ("kind = \"udp\"\n", "needs `bind`"),
            (
                "kind = \"udp\"\nbind = \"x\"\nport = 1\n",
                "port: a udp edge takes",
            ),
            ("kind = \"tcp\"\n", "one of `connect`"),
            (
                "kind = \"tcp\"\nconnect = \"a\"\nlisten = \"b\"\n",
                "one of `connect`",
            ),
            ("kind = \"serial\"\ndevice = \"/dev/x\"\n", "needs `baud`"),
            (
                "kind = \"serial\"\ndevice = \"/dev/x\"\nbaud = 9600\nframing = \"cobs\"\n",
                "framing",
            ),
            ("kind = \"pigeon\"\n", "udp, tcp, serial or custom"),
            ("bind = \"x\"\n", "needs `kind`"),
        ] {
            let err = parse(&format!("[edge.e]\n{table}")).unwrap_err();
            assert!(err.contains("[edge.e]") && err.contains(expect), "{err}");
        }
    }

    #[test]
    fn wires_with_edges_follow_the_rules() {
        let cfg = parse(
            r#"
[branch.a]
[branch.b]
[edge.x]
kind = "udp"
bind = "0.0.0.0:1"
[edge.y]
kind = "custom"
[edge.beat]
kind = "custom"
[[wire]]
from = "x"
to = ["y"]
[[wire]]
from = "x"
message = "Beat"
to = ["a"]
[[wire]]
from = "a"
message = "Beat"
to = ["x"]
[[wire]]
from = "a"
to = ["b"]
"#,
        )
        .unwrap();
        let all = check(&cfg, &messages()).errors.join("\n");
        for expected in [
            "edges don't wire to edges",
            "an edge sends what it receives, not a message",
            "to send to edge `x`, wire without a message",
            "a wire between branches carries a message",
            "edge `beat`: its input is called Beat, like a message",
        ] {
            assert!(all.contains(expected), "missing {expected:?} in:\n{all}");
        }
    }

    #[test]
    fn wiring_carries_edges_both_ways() {
        let w = render_wiring(&parse(EDGES).unwrap());
        // What edges receive, and the settings they're started with.
        assert!(
            w.contains("pub enum EdgeIn {\n    Tak(crate::bonsai::Packet),\n"),
            "{w}"
        );
        assert!(
            w.contains("    Radio(<crate::edges::radio::Radio as crate::bonsai::Edge>::In),\n"),
            "{w}"
        );
        assert!(w.contains(
            "const TAK: crate::bonsai::UdpConfig = crate::bonsai::UdpConfig {\n    bind: \"0.0.0.0:6969\",\n    to: Some(\"100.125.26.5:6970\"),\n    join: &[\"239.2.3.2\"],\n    iface: \"0.0.0.0\",\n    reply: false,\n};"
        ), "{w}");
        assert!(w.contains("framing: crate::bonsai::Framing::Lines,"), "{w}");
        assert!(w.contains("let tx = spawn_edge(\"tak\", || crate::bonsai::Udp::setup(TAK), events.clone(), EdgeIn::Tak);"), "{w}");
        assert!(
            w.contains("spawn_edge(\"gps\", || crate::edges::serial::Serial::setup(GPS),"),
            "{w}"
        );
        assert!(
            w.contains("spawn_edge(\"radio\", crate::edges::radio::Radio::setup,"),
            "{w}"
        );
        // Branches receive an edge as its CamelCase input, fanned out in order.
        assert!(
            w.contains("        /// From the tak edge.\n        Tak(crate::bonsai::Packet),"),
            "{w}"
        );
        assert!(w.contains(
            "            Event::Edge(EdgeIn::Fc(value)) => {\n                queue.extend(self.gcs.process(gcs::Input::Fc(value.clone())).sent);\n                queue.extend(self.atak.process(atak::Input::Fc(value)).sent);\n"
        ), "{w}");
        // ...and send to edges with out.to_<edge>, one Msg variant per edge.
        assert!(w.contains("pub fn to_tak(&mut self, value: crate::bonsai::Packet) {\n            self.sent.push(Msg::AtakToTak(value));"), "{w}");
        assert!(
            w.contains("Msg::AtakToFc(value) => self.fc.send(value),"),
            "{w}"
        );
        assert!(w.contains("pub fn to_radio(&mut self, value: <crate::edges::radio::Radio as crate::bonsai::Edge>::Out)"), "{w}");
        // Tests can see what the core sent an edge.
        assert!(
            w.contains("pub fn drain_tak(&mut self) -> Vec<crate::bonsai::Packet> {"),
            "{w}"
        );
    }

    #[test]
    fn custom_edge_settings_and_modules() {
        let cfg = parse(EDGES).unwrap();
        assert!(
            render_settings(&cfg).contains("pub mod radio {\n    pub const CHANNEL: i64 = 7;\n}")
        );
        let m = render_edges_mod(&cfg);
        assert!(
            m.ends_with("\npub mod radio;\npub mod serial; // bonsai's serial edge\n"),
            "{m}"
        );
        assert!(cfg.has_serial());
    }
}

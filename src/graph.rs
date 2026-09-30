//! A tree's graph and the code generated from it. Pure: text in, text out.
//!
//! `bonsai.toml` holds the graph: the branches (with their settings and
//! `rate`) and the wires between them. `src/messages.rs` holds the message
//! types. From those, `bonsai sync` writes `src/wiring.rs` (the typed inputs,
//! outputs and core loop), `src/settings.rs` and `src/branches/mod.rs`.

use std::str::FromStr;

use toml_edit::{DocumentMut, Item, Value};

/// A tree's graph, read from `bonsai.toml`.
#[derive(Debug, Default, PartialEq)]
pub struct Config {
    /// In the file's order, which is the order the core runs them in.
    pub branches: Vec<BranchCfg>,
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

#[derive(Clone, Debug, PartialEq)]
pub enum Setting {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    List(Vec<Setting>),
}

/// `from` sends `message` to every branch in `to`.
#[derive(Clone, Debug, PartialEq)]
pub struct Wire {
    pub from: String,
    pub message: String,
    pub to: Vec<String>,
}

/// Read `bonsai.toml`. Errors name the key that's wrong.
pub fn parse(src: &str) -> Result<Config, String> {
    let doc = DocumentMut::from_str(src).map_err(|e| format!("bonsai.toml: {e}"))?;
    let mut cfg = Config::default();

    if let Some(branches) = doc.get("branch") {
        let branches = branches
            .as_table_like()
            .ok_or("bonsai.toml: `branch` must be tables, like [branch.radio]")?;
        for (name, item) in branches.iter() {
            let table = item
                .as_table_like()
                .ok_or_else(|| format!("bonsai.toml: [branch.{name}] must be a table"))?;
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
    }

    if let Some(wires) = doc.get("wire") {
        let wires = wires
            .as_array_of_tables()
            .ok_or("bonsai.toml: wires are [[wire]] tables")?;
        for (i, wire) in wires.iter().enumerate() {
            let text = |key: &str| -> Result<String, String> {
                wire.get(key)
                    .and_then(Item::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| format!("bonsai.toml: wire {} needs `{key} = \"…\"`", i + 1))
            };
            let from = text("from")?;
            let message = text("message")?;
            let to = wire
                .get("to")
                .and_then(Item::as_array)
                .ok_or_else(|| format!("bonsai.toml: wire {} needs `to = [\"…\"]`", i + 1))?
                .iter()
                .map(|v| {
                    v.as_str().map(str::to_string).ok_or_else(|| {
                        format!("bonsai.toml: wire {}: `to` lists branch names", i + 1)
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            cfg.wires.push(Wire { from, message, to });
        }
    }
    Ok(cfg)
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

/// `radio_link` → `RadioLink`: a branch's struct name.
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

/// A usable branch name: snake_case, and not a Rust keyword.
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

/// Check the graph against itself and the declared messages.
pub fn check(cfg: &Config, messages: &[String]) -> Report {
    let mut r = Report::default();
    let is_branch = |n: &str| cfg.branches.iter().any(|b| b.name == n);

    for (i, b) in cfg.branches.iter().enumerate() {
        if !is_branch_name(&b.name) {
            r.errors.push(format!(
                "branch `{}`: a name is snake_case (a-z, 0-9, _) and not a Rust keyword",
                b.name
            ));
        }
        if cfg.branches[..i]
            .iter()
            .any(|o| camel(&o.name) == camel(&b.name))
        {
            r.errors.push(format!(
                "branches `{}` and another share the struct name {}",
                b.name,
                camel(&b.name)
            ));
        }
        for (key, _) in &b.settings {
            if !is_setting_key(key) {
                r.errors.push(format!(
                    "[branch.{}] {key}: a setting name is letters, digits and _",
                    b.name
                ));
            }
        }
    }

    for (i, w) in cfg.wires.iter().enumerate() {
        let wire = format!("wire {} --{}-->", w.from, w.message);
        if !is_branch(&w.from) {
            r.errors.push(format!("{wire}: no branch `{}`", w.from));
        }
        if !messages.contains(&w.message) {
            r.errors.push(format!(
                "{wire}: no message `{}` in src/messages.rs",
                w.message
            ));
        }
        if w.message == "Tick" {
            r.errors.push(format!(
                "{wire}: `Tick` is the input a branch's rate makes; rename the message"
            ));
        }
        if w.to.is_empty() {
            r.errors.push(format!("{wire}: `to` is empty"));
        }
        for (j, to) in w.to.iter().enumerate() {
            if !is_branch(to) {
                r.errors.push(format!("{wire} {to}: no branch `{to}`"));
            }
            if *to == w.from {
                r.errors.push(format!(
                    "{wire} {to}: a branch can't send to itself; keep that state in the branch"
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
                "{} has no inputs, so its process never runs: wire a message to it, or give it a rate",
                b.name
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

/// The messages each branch receives, in wire order, without repeats.
fn inputs_of<'a>(cfg: &'a Config, branch: &str) -> Vec<&'a str> {
    let mut inputs: Vec<&str> = Vec::new();
    for w in &cfg.wires {
        if w.to.iter().any(|t| t == branch) && !inputs.contains(&w.message.as_str()) {
            inputs.push(&w.message);
        }
    }
    inputs
}

/// A wire's variant in `Msg`: `PulseBeat` for pulse sending Beat.
fn msg_variant(w: &Wire) -> String {
    format!("{}{}", camel(&w.from), w.message)
}

const HEADER: &str = "//!\n\
//! Generated by `bonsai sync` from bonsai.toml and src/messages.rs. Don't edit\n\
//! it: change those, then run `bonsai sync` (every bonsai command that changes\n\
//! the graph runs it for you).\n";

/// `src/wiring.rs`: each branch's `Input` and `Out`, and the core that
/// delivers every message. Assumes `check` found no errors.
pub fn render_wiring(cfg: &Config) -> String {
    let mut o = format!("//! The wiring between branches.\n{HEADER}");
    o.push_str(
        "#![allow(dead_code, unused_imports)]

use std::collections::VecDeque;

use crate::bonsai::{Event, Sends, Slot, Tree, drain};
use crate::branches;
use crate::messages::*;

/// Every message in flight: one variant per wire.
#[derive(Debug)]
pub enum Msg {
",
    );
    for w in &cfg.wires {
        o.push_str(&format!(
            "    /// {} → {}\n    {}({}),\n",
            w.from,
            w.to.join(", "),
            msg_variant(w),
            w.message
        ));
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
        if b.rate.is_some() {
            o.push_str("        /// Its `rate` ticked.\n        Tick,\n");
        }
        for m in inputs_of(cfg, &b.name) {
            o.push_str(&format!("        {m}({m}),\n"));
        }
        o.push_str(&format!(
            "    }}

    /// Where {name} sends: `out.send(message)`.
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

        /// Everything sent so far, oldest first: for tests.
        pub fn sent(&self) -> &[Msg] {{
            &self.sent
        }}
    }}
",
            name = b.name
        ));
        for w in cfg.wires.iter().filter(|w| w.from == b.name) {
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
                m = w.message,
                v = msg_variant(w)
            ));
        }
        o.push_str("}\n");
    }

    o.push_str("\n/// Every branch, set up and waiting for events.\npub struct Core {\n");
    for b in &cfg.branches {
        o.push_str(&format!(
            "    {}: Slot<branches::{}::{}>,\n",
            b.name,
            b.name,
            camel(&b.name)
        ));
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
    o.push_str(
        "            queue: VecDeque::new(),
        }
    }

    /// Hand one message to every branch wired to it, in bonsai.toml's order.
    fn deliver(&mut self, message: Msg, queue: &mut VecDeque<Msg>) {
        match message {
",
    );
    for w in &cfg.wires {
        o.push_str(&format!(
            "            Msg::{}(message) => {{\n",
            msg_variant(w)
        ));
        for (i, to) in w.to.iter().enumerate() {
            let value = if i + 1 == w.to.len() {
                "message"
            } else {
                "message.clone()"
            };
            o.push_str(&format!(
                "                queue.extend(self.{to}.process({to}::Input::{m}({value})).sent);\n",
                m = w.message
            ));
        }
        o.push_str("            }\n");
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
        "]
    }

    fn handle(&mut self, event: Event) {
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
    o.push_str(
        "            Event::Tick(_) => {}
        }
        drain(&mut queue, |message, queue| self.deliver(message, queue));
        self.queue = queue;
    }
}
",
    );
    o
}

/// `src/settings.rs`: each branch's `[branch.<name>]` values as constants.
pub fn render_settings(cfg: &Config) -> String {
    let mut o = format!(
        "//! Each branch's settings, from its [branch.<name>] table.\n{HEADER}\
         #![allow(dead_code)]\n"
    );
    for b in cfg.branches.iter().filter(|b| !b.settings.is_empty()) {
        o.push_str(&format!("\npub mod {} {{\n", b.name));
        for (key, value) in &b.settings {
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
    let mut o = format!("//! The branches.\n{HEADER}\n");
    for b in &cfg.branches {
        o.push_str(&format!("pub mod {};\n", b.name));
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
message = "Packet"
to = ["log"]
"#;

    fn messages() -> Vec<String> {
        vec!["Beat".to_string(), "Packet".to_string()]
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
                message: "Beat".to_string(),
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
        let err = parse("[[wire]]\nfrom = \"a\"\nto = [\"b\"]\n").unwrap_err();
        assert!(err.contains("message"), "{err}");
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
            "no branch `ghost`",
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
message = "Packet"
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
            w.contains("        Beat(Beat),\n        Packet(Packet),\n    }"),
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
            m.ends_with("\npub mod pulse;\npub mod radio;\npub mod log;\n"),
            "{m}"
        );
    }
}

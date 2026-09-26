//! The sap generator. Turns a tree's wiring — the `Nutrient` enum, each
//! nutrient's path config in `bonsai.toml`, and every node's taps/releases —
//! into the generated `src/sap.rs`, plus the analysis `list`, the wiring
//! warnings, and `feed`'s size hints are built on.
//!
//! Everything here is pure (strings in, strings out) so it's unit-testable; the
//! fs orchestration lives in `main.rs` (`sync_sap`).

use std::collections::BTreeMap;
use std::fmt::Write as _;

// ---------------------------------------------------------------------------
// Path config: shape + capacity per nutrient, and the tree-wide layout.
// ---------------------------------------------------------------------------

/// Which Embassy primitive carries a nutrient.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shape {
    /// `PubSubChannel`: one → many. Releasing never waits; a tapper that falls
    /// behind loses the oldest (and the loss is counted). Events.
    Broadcast,
    /// `Channel`: many → one. Releasing waits while the queue is full, so
    /// nothing is dropped. Commands, TX queues.
    Directed,
    /// `Watch`: the latest value. Releasing overwrites; a tapper that arrives
    /// late still gets the current value. Link status, armed state, position.
    State,
}

pub const SHAPES: [Shape; 3] = [Shape::Broadcast, Shape::Directed, Shape::State];

impl Shape {
    pub fn parse(s: &str) -> Option<Shape> {
        SHAPES.into_iter().find(|sh| sh.name() == s)
    }

    pub fn name(self) -> &'static str {
        match self {
            Shape::Broadcast => "broadcast",
            Shape::Directed => "directed",
            Shape::State => "state",
        }
    }

    pub fn default_cap(self) -> usize {
        match self {
            Shape::Broadcast => 4,
            Shape::Directed => 8,
            Shape::State => 1, // a Watch holds exactly one value
        }
    }

    /// Whether `sap.release(..).await` can wait on this shape.
    pub fn blocks(self) -> bool {
        self == Shape::Directed
    }
}

/// How the whole tree's sap is laid out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layout {
    /// One channel per nutrient (the default).
    Paths,
    /// One shared broadcast bus for every nutrient — the classic single trunk.
    Trunk,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PathCfg {
    pub shape: Shape,
    pub cap: usize,
}

impl PathCfg {
    pub fn new(shape: Shape, cap: Option<usize>) -> PathCfg {
        let cap = match shape {
            Shape::State => 1,
            _ => cap.unwrap_or(shape.default_cap()),
        };
        PathCfg { shape, cap }
    }
}

impl Default for PathCfg {
    fn default() -> Self {
        PathCfg::new(Shape::Broadcast, None)
    }
}

/// `bonsai.toml`, parsed. Nutrients without an entry get the default path.
pub struct Config {
    pub layout: Layout,
    pub paths: BTreeMap<String, PathCfg>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            layout: Layout::Paths,
            paths: BTreeMap::new(),
        }
    }
}

impl Config {
    pub fn parse(src: &str) -> Result<Config, String> {
        use std::str::FromStr;
        let doc = toml_edit::DocumentMut::from_str(src).map_err(|e| e.to_string())?;
        let mut cfg = Config::default();
        if let Some(layout) = doc.get("sap").and_then(|s| s.get("layout")) {
            cfg.layout = match layout.as_str() {
                Some("paths") => Layout::Paths,
                Some("trunk") => Layout::Trunk,
                _ => {
                    return Err(format!(
                        "[sap] layout must be \"paths\" or \"trunk\", got {layout}"
                    ));
                }
            };
        }
        if let Some(table) = doc.get("nutrients").and_then(|t| t.as_table_like()) {
            for (name, entry) in table.iter() {
                let shape = match entry.get("shape") {
                    None => Shape::Broadcast,
                    Some(s) => s.as_str().and_then(Shape::parse).ok_or_else(|| {
                        format!(
                            "[nutrients.{name}] shape must be \"broadcast\", \"directed\" or \"state\""
                        )
                    })?,
                };
                let cap = match entry.get("cap") {
                    None => None,
                    Some(c) => match c.as_integer() {
                        Some(n) if n >= 1 => Some(n as usize),
                        _ => {
                            return Err(format!(
                                "[nutrients.{name}] cap must be a positive integer"
                            ));
                        }
                    },
                };
                cfg.paths.insert(name.to_string(), PathCfg::new(shape, cap));
            }
        }
        Ok(cfg)
    }

    pub fn path(&self, nutrient: &str) -> PathCfg {
        self.paths.get(nutrient).copied().unwrap_or_default()
    }
}

/// Set `[nutrients.<name>]` in a `bonsai.toml` source, keeping its comments.
pub fn set_path(src: &str, name: &str, cfg: PathCfg) -> Result<String, String> {
    use std::str::FromStr;
    use toml_edit::{DocumentMut, Item, Table, value};
    let mut doc = DocumentMut::from_str(src).map_err(|e| e.to_string())?;
    if !doc.contains_key("nutrients") {
        let mut t = Table::new();
        t.set_implicit(true); // render as `[nutrients.X]`, not an empty `[nutrients]`
        doc.insert("nutrients", Item::Table(t));
    }
    let nutrients = doc["nutrients"]
        .as_table_mut()
        .ok_or("`nutrients` in bonsai.toml must be a table")?;
    if !nutrients.contains_key(name) {
        nutrients.insert(name, Item::Table(Table::new()));
    }
    let entry = nutrients[name]
        .as_table_mut()
        .ok_or_else(|| format!("[nutrients.{name}] in bonsai.toml must be a table"))?;
    entry["shape"] = value(cfg.shape.name());
    if cfg.shape == Shape::State {
        entry.remove("cap"); // a Watch holds one value; a cap would only mislead
    } else {
        entry["cap"] = value(cfg.cap as i64);
    }
    Ok(doc.to_string())
}

/// Drop `[nutrients.<name>]` from a `bonsai.toml` source (no-op if absent).
pub fn remove_path(src: &str, name: &str) -> Result<String, String> {
    use std::str::FromStr;
    let mut doc = toml_edit::DocumentMut::from_str(src).map_err(|e| e.to_string())?;
    if let Some(t) = doc.get_mut("nutrients").and_then(|t| t.as_table_mut()) {
        t.remove(name);
    }
    Ok(doc.to_string())
}

// ---------------------------------------------------------------------------
// The Nutrient enum, parsed well enough to name variants and size them.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Variant {
    pub name: String,
    /// Field types, in order (struct fields or tuple members). Empty = unit.
    pub fields: Vec<String>,
    /// Field names for a struct variant (`Foo { a: A }`); None for a tuple or
    /// unit variant.
    pub names: Option<Vec<String>>,
}

/// Split `s` on commas that aren't nested inside ()/[]/{}/<>.
fn split_top_level(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0i32, 0);
    for (i, c) in s.char_indices() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

/// Drop leading `#[…]` attributes from a variant or field.
fn strip_attrs(mut s: &str) -> &str {
    loop {
        s = s.trim_start();
        let Some(rest) = s.strip_prefix("#[") else {
            return s;
        };
        let mut depth = 1;
        let mut end = rest.len();
        for (i, c) in rest.char_indices() {
            match c {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        s = &rest[end..];
    }
}

/// The variants of `pub enum Nutrient` in trunk.rs — unit, struct and tuple,
/// single- or multi-line. Comments (docs, the marker) are ignored.
pub fn parse_variants(trunk: &str) -> Vec<Variant> {
    // Strip line comments, then take the enum body between its braces.
    let code: String = trunk
        .lines()
        .map(|l| l.find("//").map_or(l, |i| &l[..i]))
        .collect::<Vec<_>>()
        .join("\n");
    let Some(start) = code.find("pub enum Nutrient") else {
        return Vec::new();
    };
    let Some(open) = code[start..].find('{').map(|i| start + i + 1) else {
        return Vec::new();
    };
    let mut depth = 1;
    let mut close = code.len();
    for (i, c) in code[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    close = open + i;
                    break;
                }
            }
            _ => {}
        }
    }

    let mut out = Vec::new();
    for item in split_top_level(&code[open..close]) {
        let item = strip_attrs(item).trim();
        let name: String = item
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if name.is_empty() {
            continue;
        }
        let rest = item[name.len()..].trim();
        let inner = |open: char, close: char| {
            rest.strip_prefix(open)
                .and_then(|r| r.rfind(close).map(|i| &r[..i]))
                .unwrap_or("")
        };
        let (fields, names) = if rest.starts_with('{') {
            let pairs: Vec<(String, String)> = split_top_level(inner('{', '}'))
                .into_iter()
                .filter_map(|f| {
                    let f = strip_attrs(f).trim();
                    let f = f.strip_prefix("pub ").unwrap_or(f);
                    f.split_once(':')
                        .map(|(n, ty)| (n.trim().to_string(), ty.trim().to_string()))
                })
                .collect();
            let (names, fields) = pairs.into_iter().unzip();
            (fields, Some(names))
        } else if rest.starts_with('(') {
            let fields = split_top_level(inner('(', ')'))
                .into_iter()
                .map(|t| strip_attrs(t).trim().to_string())
                .filter(|t| !t.is_empty())
                .collect();
            (fields, None)
        } else {
            (Vec::new(), None)
        };
        out.push(Variant {
            name,
            fields,
            names,
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Size estimates — every slot of every path is sized for the largest variant.
// Rough by design (no layout optimisations beyond field reordering), so the CLI
// always prints them with a `≈`.
// ---------------------------------------------------------------------------

/// (size, align) of a type, or None if it isn't one we can size from text.
/// `ptr` is the target's pointer width in bytes.
pub fn type_size(ty: &str, ptr: usize) -> Option<(usize, usize)> {
    let ty = ty.trim();
    let prim = match ty {
        "u8" | "i8" | "bool" => Some((1, 1)),
        "u16" | "i16" => Some((2, 2)),
        "u32" | "i32" | "f32" | "char" => Some((4, 4)),
        "u64" | "i64" | "f64" => Some((8, 8.min(ptr * 2))),
        "u128" | "i128" => Some((16, 16.min(ptr * 2))),
        "usize" | "isize" => Some((ptr, ptr)),
        "()" => Some((0, 1)),
        _ => None,
    };
    if prim.is_some() {
        return prim;
    }
    // References, Box, &str and slices: a (possibly fat) pointer.
    if let Some(inner) = ty.strip_prefix('&').or_else(|| generic_arg(ty, "Box")) {
        let inner = inner.trim_start_matches("'static").trim();
        let inner = inner.strip_prefix("mut ").unwrap_or(inner).trim();
        let fat = inner == "str" || (inner.starts_with('[') && !inner.contains(';'));
        return Some((if fat { 2 * ptr } else { ptr }, ptr));
    }
    // [T; N]
    if let Some(body) = ty.strip_prefix('[').and_then(|r| r.strip_suffix(']'))
        && let Some((elem, n)) = body.rsplit_once(';')
    {
        let (size, align) = type_size(elem, ptr)?;
        return Some((size * n.trim().parse::<usize>().ok()?, align));
    }
    // (A, B, …)
    if let Some(body) = ty.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
        let members: Vec<&str> = split_top_level(body)
            .into_iter()
            .filter(|m| !m.trim().is_empty())
            .collect();
        return struct_size(&members, ptr);
    }
    // heapless::Vec<T, N> / heapless::String<N> (any path prefix).
    if let Some(args) = generic_arg(ty, "Vec") {
        let parts = split_top_level(args);
        if let [elem, n] = parts.as_slice() {
            let (size, align) = type_size(elem, ptr)?;
            let n: usize = n.trim().parse().ok()?;
            return Some((round_up(size * n + ptr, align.max(ptr)), align.max(ptr)));
        }
        return Some((3 * ptr, ptr)); // std Vec<T>: ptr + len + cap
    }
    if let Some(n) = generic_arg(ty, "String") {
        let n: usize = n.trim().parse().ok()?;
        return Some((round_up(n + ptr, ptr), ptr));
    }
    if ty == "String" || ty.ends_with("::String") {
        return Some((3 * ptr, ptr));
    }
    if let Some(inner) = generic_arg(ty, "Option") {
        let (size, align) = type_size(inner, ptr)?;
        // Niche-optimised for pointers; otherwise a tag padded to the alignment.
        let niche = inner.trim_start().starts_with('&') || generic_arg(inner, "Box").is_some();
        return Some(if niche {
            (size, align)
        } else {
            (round_up(size + 1, align), align)
        });
    }
    None
}

/// The `<…>` argument of `Name<…>` (with any path prefix), if `ty` is that type.
fn generic_arg<'a>(ty: &'a str, name: &str) -> Option<&'a str> {
    let lt = ty.find('<')?;
    let head = ty[..lt].trim();
    let last = head.rsplit("::").next().unwrap_or(head);
    (last == name && ty.ends_with('>')).then(|| &ty[lt + 1..ty.len() - 1])
}

fn round_up(n: usize, align: usize) -> usize {
    if align == 0 {
        n
    } else {
        n.div_ceil(align) * align
    }
}

/// Size of a struct/tuple from its member types, with Rust's field reordering
/// (largest alignment first) approximated.
fn struct_size(members: &[&str], ptr: usize) -> Option<(usize, usize)> {
    let mut sized: Vec<(usize, usize)> = members
        .iter()
        .map(|m| type_size(m, ptr))
        .collect::<Option<_>>()?;
    sized.sort_by_key(|s| std::cmp::Reverse(s.1));
    let align = sized.iter().map(|s| s.1).max().unwrap_or(1);
    let mut size = 0;
    for (s, a) in sized {
        size = round_up(size, a) + s;
    }
    Some((round_up(size, align), align))
}

/// Estimated payload size of one variant, or None if a field type is unknown.
pub fn variant_size(v: &Variant, ptr: usize) -> Option<(usize, usize)> {
    let members: Vec<&str> = v.fields.iter().map(String::as_str).collect();
    struct_size(&members, ptr)
}

/// Estimated size of the whole `Nutrient` enum (largest payload + tag), or None
/// if any variant can't be sized.
pub fn enum_size(variants: &[Variant], ptr: usize) -> Option<usize> {
    let sizes: Vec<(usize, usize)> = variants
        .iter()
        .map(|v| variant_size(v, ptr))
        .collect::<Option<_>>()?;
    let align = sizes.iter().map(|s| s.1).max().unwrap_or(1);
    let payload = sizes.iter().map(|s| s.0).max().unwrap_or(0);
    Some(round_up(payload + 1, align).max(1))
}

// ---------------------------------------------------------------------------
// The graph: nodes (the pulse + every branch) and one path per nutrient.
// ---------------------------------------------------------------------------

/// A file that taps and/or releases nutrients: `pulse` or a branch.
pub struct Node {
    pub name: String,
    pub taps: Vec<String>,
    pub releases: Vec<String>,
    /// Releases made with `sap.release(..).await` (can wait), not `try_release`.
    pub awaited: Vec<String>,
    /// Whether the file uses its generated `sap::<name>::Taps`.
    pub uses_taps: bool,
}

impl Node {
    /// Scan a node's source for its connections to each known nutrient.
    pub fn scan(name: &str, src: &str, nutrients: &[String]) -> Node {
        let having = |f: &dyn Fn(&str, &str) -> bool| -> Vec<String> {
            nutrients
                .iter()
                .filter(|n| src.lines().any(|l| f(l, n.as_str())))
                .cloned()
                .collect()
        };
        Node {
            name: name.to_string(),
            taps: having(&|l, n| crate::is_arm_for(l, n)),
            releases: having(&|l, n| crate::mentions_release_of(l, n)),
            awaited: having(&|l, n| crate::mentions_awaited_release_of(l, n)),
            uses_taps: src.contains(&format!("sap::{name}::Taps")),
        }
    }
}

pub struct Path {
    pub nutrient: String,
    /// The variant's shape, so the path can carry just its own payload.
    pub variant: Variant,
    pub cfg: PathCfg,
    pub releasers: Vec<String>,
    pub tappers: Vec<String>,
}

/// How `sap.rs` reports a lagging tapper — the tree's own logger.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Logger {
    Defmt,
    Std,
}

pub struct Graph {
    pub layout: Layout,
    pub paths: Vec<Path>,
    pub nodes: Vec<Node>,
    pub logger: Logger,
}

impl Graph {
    pub fn build(variants: &[Variant], cfg: &Config, nodes: Vec<Node>, logger: Logger) -> Graph {
        let who = |f: &dyn Fn(&Node) -> &Vec<String>, n: &str| -> Vec<String> {
            nodes
                .iter()
                .filter(|node| f(node).iter().any(|x| x == n))
                .map(|node| node.name.clone())
                .collect()
        };
        let paths = variants
            .iter()
            .map(|v| (&v.name, v))
            .map(|(n, v)| Path {
                nutrient: n.clone(),
                variant: v.clone(),
                cfg: cfg.path(n),
                releasers: who(&|node| &node.releases, n),
                tappers: who(&|node| &node.taps, n),
            })
            .collect();
        Graph {
            layout: cfg.layout,
            paths,
            nodes,
            logger,
        }
    }

    /// Estimated RAM the sap's queues hold, or None if a payload type can't be
    /// sized. Paths layout: each path's slots hold only its own payload; trunk
    /// layout: every slot holds a whole `Nutrient`.
    pub fn sap_bytes(&self, ptr: usize) -> Option<usize> {
        match self.layout {
            Layout::Trunk => {
                let variants: Vec<Variant> = self.paths.iter().map(|p| p.variant.clone()).collect();
                Some(self.trunk_cap() * enum_size(&variants, ptr)?)
            }
            Layout::Paths => self
                .paths
                .iter()
                .map(|p| Some(p.cfg.cap * variant_size(&p.variant, ptr)?.0))
                .sum(),
        }
    }

    fn path(&self, nutrient: &str) -> Option<&Path> {
        self.paths.iter().find(|p| p.nutrient == nutrient)
    }

    /// The shared bus's capacity in the trunk layout: the largest queued cap.
    pub fn trunk_cap(&self) -> usize {
        self.paths
            .iter()
            .filter(|p| p.cfg.shape != Shape::State)
            .map(|p| p.cfg.cap)
            .max()
            .unwrap_or(Shape::Broadcast.default_cap())
    }

    /// Nodes that subscribe to the shared bus in the trunk layout.
    pub fn trunk_tappers(&self) -> usize {
        self.nodes.iter().filter(|n| !n.taps.is_empty()).count()
    }

    /// One line per path for docs and `list`:
    /// `Beat   broadcast  cap 2    pulse → pulse`.
    pub fn flow_lines(&self) -> Vec<String> {
        let w = self
            .paths
            .iter()
            .map(|p| p.nutrient.len())
            .max()
            .unwrap_or(0);
        self.paths
            .iter()
            .map(|p| {
                let (shape, cap) = match (self.layout, p.cfg.shape) {
                    (Layout::Trunk, _) => ("trunk".to_string(), String::new()),
                    (_, Shape::State) => (p.cfg.shape.name().to_string(), "latest".to_string()),
                    (_, s) => (s.name().to_string(), format!("cap {}", p.cfg.cap)),
                };
                let list = |v: &Vec<String>| {
                    if v.is_empty() {
                        "·".to_string()
                    } else {
                        v.join(", ")
                    }
                };
                format!(
                    "{:<w$}  {:<9}  {:<7}  {} → {}",
                    p.nutrient,
                    shape,
                    cap,
                    list(&p.releasers),
                    list(&p.tappers),
                )
                .trim_end()
                .to_string()
            })
            .collect()
    }

    /// Wiring hazards worth telling the user about.
    pub fn warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        // Feedback: a node that taps what it releases can feed itself forever
        // (a duplex loop releases after *every* nutrient it takes). Directed +
        // awaited is the worse case — a deadlock — and is reported below.
        // (The pulse is exempt: its heartbeat and monitor are separate tasks.)
        for node in self.nodes.iter().filter(|n| n.name != "pulse") {
            for n in node.taps.iter().filter(|n| node.releases.contains(n)) {
                let deadlock = self.layout == Layout::Paths
                    && node.awaited.contains(n)
                    && self.path(n).is_some_and(|p| p.cfg.shape.blocks());
                if !deadlock {
                    out.push(format!(
                        "`{}` taps and releases `{n}` — if it releases `{n}` in reply to every \
                         nutrient it takes (a duplex loop's default), it feeds itself forever. \
                         Release it only under a condition, inside the match arm.",
                        node.name
                    ));
                }
            }
        }
        if self.layout == Layout::Trunk {
            let shaped: Vec<&str> = self
                .paths
                .iter()
                .filter(|p| p.cfg.shape != Shape::Broadcast)
                .map(|p| p.nutrient.as_str())
                .collect();
            if !shaped.is_empty() {
                out.push(format!(
                    "layout = \"trunk\" carries every nutrient on one broadcast bus — the \
                     directed/state shape of {} is ignored. Switch [sap] layout to \"paths\" to use it.",
                    shaped.join(", ")
                ));
            }
            return out;
        }
        for p in &self.paths {
            if p.cfg.shape == Shape::Directed && p.tappers.len() > 1 {
                out.push(format!(
                    "`{}` is directed but tapped by {} — each nutrient reaches only one of them. \
                     Make it broadcast if they should all see it.",
                    p.nutrient,
                    p.tappers.join(" and ")
                ));
            }
        }
        // Deadlock: a cycle of awaited releases on directed paths. Each edge is
        // "releaser waits for tapper to drain". A self-loop is the classic case.
        let edges: Vec<(&str, &str, &str)> = self
            .nodes
            .iter()
            .flat_map(|node| {
                node.awaited.iter().filter_map(move |n| {
                    let p = self.path(n)?;
                    p.cfg.shape.blocks().then_some((node, p))
                })
            })
            .flat_map(|(node, p)| {
                p.tappers
                    .iter()
                    .map(move |t| (node.name.as_str(), t.as_str(), p.nutrient.as_str()))
            })
            .collect();
        for &(from, to, n) in &edges {
            if from == to {
                out.push(format!(
                    "deadlock risk: `{from}` taps and releases directed `{n}`. \
                     `sap.release(..).await` waits while that path is full — and the only task \
                     that drains it is `{from}` itself. Use `sap.try_release(..)` there, or make \
                     `{n}` broadcast."
                ));
            }
        }
        for cycle in cycles(&edges) {
            out.push(format!(
                "deadlock risk: awaited directed releases form a cycle {} — if every path in it \
                 fills, each branch waits on the next forever. Break it with `sap.try_release(..)` \
                 or a broadcast path.",
                cycle.join(" → ")
            ));
        }
        out
    }
}

/// Simple cycles (length ≥ 2) among branches linked by directed-path edges,
/// each reported once from its lexicographically smallest node.
fn cycles(edges: &[(&str, &str, &str)]) -> Vec<Vec<String>> {
    let mut adj: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for &(from, to, _) in edges {
        if from != to {
            let next = adj.entry(from).or_default();
            if !next.contains(&to) {
                next.push(to);
            }
        }
    }
    let mut found = Vec::new();
    for &start in adj.keys() {
        // DFS for paths back to `start` through nodes greater than `start`.
        let mut stack: Vec<(Vec<&str>, usize)> = vec![(vec![start], 0)];
        while let Some((path, i)) = stack.pop() {
            let last = *path.last().unwrap();
            let Some(next) = adj.get(last).and_then(|v| v.get(i)) else {
                continue;
            };
            stack.push((path.clone(), i + 1));
            if *next == start && path.len() > 1 {
                let mut c: Vec<String> = path.iter().map(|s| s.to_string()).collect();
                c.push(start.to_string());
                found.push(c);
            } else if *next > start && !path.contains(next) {
                let mut p = path.clone();
                p.push(next);
                stack.push((p, 0));
            }
        }
    }
    found
}

// ---------------------------------------------------------------------------
// Rendering `src/sap.rs`.
// ---------------------------------------------------------------------------

/// `LinkUp` → `link_up`, `GPSFix` → `gps_fix`.
pub fn snake(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
            if prev.is_ascii_lowercase()
                || prev.is_ascii_digit()
                || (prev.is_ascii_uppercase() && next_lower)
            {
                out.push('_');
            }
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

fn static_name(nutrient: &str) -> String {
    format!("{}_PATH", snake(nutrient).to_ascii_uppercase())
}

const HEADER: &str = "\
//! The sap — how nutrients flow between branches. GENERATED by bonsai from the
//! tree's wiring: every wiring command (branch/snip/feed/starve/tap/untap/
//! release/unrelease/path/sync) rewrites this file, so don't edit it by hand.
//! To change a path's shape or capacity, run `bonsai path <Nutrient> …` (or edit
//! `bonsai.toml`, then `bonsai sync`).
//!
//! Branches never touch the channels directly. A branch that feeds nutrients
//! holds a `Sap` (`trunk.sap()`) and calls `sap.release(..)`; one that taps
//! them owns its generated `Taps` (`sap::<branch>::Taps::new()`) and awaits
//! `taps.next()`, which wakes only for the nutrients that branch taps.
";

/// Render the whole `src/sap.rs` for a graph.
pub fn render(g: &Graph) -> String {
    let mut o = String::from(HEADER);
    o.push_str("//!\n");
    match g.layout {
        Layout::Paths => {
            o.push_str("//! layout: paths — one channel per nutrient.\n");
        }
        Layout::Trunk => {
            let _ = writeln!(
                o,
                "//! layout: trunk — one shared bus (cap {}) carries every nutrient.",
                g.trunk_cap()
            );
        }
    }
    o.push_str("//!\n");
    for l in g.flow_lines() {
        let _ = writeln!(o, "//!   {l}");
    }
    o.push_str(
        "
#![allow(dead_code, unused_imports, clippy::new_without_default)]

use core::future::{Future, poll_fn};
use core::pin::{Pin, pin};
use core::task::{Context, Poll};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex as M;
use embassy_sync::channel::{Channel, SendFuture, TrySendError};
use embassy_sync::pubsub::{PubSubChannel, Subscriber, WaitResult};
use embassy_sync::watch::{self, Watch, WatchBehavior};

// The trunk's own imports too, so payload types written in trunk.rs resolve
// here (the sap is a child module of the trunk).
use super::*;
use crate::trunk::Nutrient;

",
    );
    match g.layout {
        Layout::Paths => render_paths(g, &mut o),
        Layout::Trunk => render_trunk(g, &mut o),
    }
    render_common(g, &mut o);
    o
}

/// `Nutrient::X { .. }` — matches unit, tuple and struct variants alike.
/// The pattern matching any value of a variant, in the form clippy expects:
/// `Nutrient::X` (unit), `Nutrient::X { .. }` (struct), `Nutrient::X(..)` (tuple).
fn pat(v: &Variant) -> String {
    let n = &v.name;
    match (&v.names, v.fields.is_empty()) {
        (_, true) => format!("Nutrient::{n}"),
        (Some(_), false) => format!("Nutrient::{n} {{ .. }}"),
        (None, false) => format!("Nutrient::{n}(..)"),
    }
}

/// What a path's slots hold: just that variant's payload. `()` for a unit
/// variant, else a generated `<Name>Slot` mirroring its fields.
fn slot_ty(p: &Path) -> String {
    if p.variant.fields.is_empty() {
        "()".into()
    } else {
        format!("{}Slot", p.nutrient)
    }
}

/// (pattern over `Nutrient`, expression building the slot) — moving the
/// payload out of the enum onto its path.
fn to_slot(p: &Path) -> (String, String) {
    let (n, v) = (&p.nutrient, &p.variant);
    if v.fields.is_empty() {
        return (pat(v), "()".into());
    }
    match &v.names {
        Some(names) => {
            let f = names.join(", ");
            (
                format!("Nutrient::{n} {{ {f} }}"),
                format!("{n}Slot {{ {f} }}"),
            )
        }
        None => {
            let f: Vec<String> = (0..v.fields.len()).map(|i| format!("f{i}")).collect();
            let f = f.join(", ");
            (format!("Nutrient::{n}({f})"), format!("{n}Slot({f})"))
        }
    }
}

/// `<name>_nutrient`: rebuild the `Nutrient` from a slot taken off its path.
fn unslot_fn(p: &Path) -> String {
    format!("{}_nutrient", snake(&p.nutrient))
}

fn render_slots(g: &Graph, o: &mut String) {
    o.push_str("// ── slots: each path holds only its own payload ────────────────────────────\n");
    for p in &g.paths {
        let (n, v) = (&p.nutrient, &p.variant);
        let f = unslot_fn(p);
        if v.fields.is_empty() {
            let _ = writeln!(o, "\nfn {f}(_: ()) -> Nutrient {{\n    Nutrient::{n}\n}}");
            continue;
        }
        match &v.names {
            Some(names) => {
                let _ = writeln!(
                    o,
                    "\n/// `{n}`'s payload, as its path carries it.\n#[derive(Clone)]\npub struct {n}Slot {{"
                );
                for (name, ty) in names.iter().zip(&v.fields) {
                    let _ = writeln!(o, "    pub {name}: {ty},");
                }
                let fields = names.join(", ");
                let _ = writeln!(
                    o,
                    "}}\n\nfn {f}({n}Slot {{ {fields} }}: {n}Slot) -> Nutrient {{\n    Nutrient::{n} {{ {fields} }}\n}}"
                );
            }
            None => {
                let tys: Vec<String> = v.fields.iter().map(|t| format!("pub {t}")).collect();
                let vars: Vec<String> = (0..v.fields.len()).map(|i| format!("f{i}")).collect();
                let vars = vars.join(", ");
                let _ = writeln!(
                    o,
                    "\n/// `{n}`'s payload, as its path carries it.\n#[derive(Clone)]\npub struct {n}Slot({});\n\nfn {f}({n}Slot({vars}): {n}Slot) -> Nutrient {{\n    Nutrient::{n}({vars})\n}}",
                    tys.join(", ")
                );
            }
        }
    }
    o.push('\n');
}

fn broadcast_ty(kind: &str, p: &Path) -> String {
    format!(
        "{kind}<'static, M, {}, {}, {}, 0>",
        slot_ty(p),
        p.cfg.cap,
        p.tappers.len()
    )
}

fn render_paths(g: &Graph, o: &mut String) {
    render_slots(g, o);
    o.push_str(
        "// ── paths: one per nutrient ────────────────────────────────────────────────\n\n",
    );
    for p in &g.paths {
        let s = static_name(&p.nutrient);
        let t = slot_ty(p);
        let n = p.tappers.len();
        let tappers = if n == 1 { "tapper" } else { "tappers" };
        match p.cfg.shape {
            Shape::Broadcast => {
                let _ = writeln!(
                    o,
                    "/// `{}`: broadcast, {} queued, {n} {tappers}.",
                    p.nutrient, p.cfg.cap
                );
                let _ = writeln!(
                    o,
                    "static {s}: PubSubChannel<M, {t}, {}, {n}, 0> = PubSubChannel::new();",
                    p.cfg.cap
                );
            }
            Shape::Directed => {
                let _ = writeln!(
                    o,
                    "/// `{}`: directed, {} queued, {n} {tappers}.",
                    p.nutrient, p.cfg.cap
                );
                let _ = writeln!(
                    o,
                    "static {s}: Channel<M, {t}, {}> = Channel::new();",
                    p.cfg.cap
                );
            }
            Shape::State => {
                let _ = writeln!(
                    o,
                    "/// `{}`: state (latest value), {n} {tappers}.",
                    p.nutrient
                );
                let _ = writeln!(o, "static {s}: Watch<M, {t}, {n}> = Watch::new();");
            }
        }
    }

    // Sap::release / try_release: move each variant's payload onto its path.
    let mut release = String::new();
    let mut try_release = String::new();
    let mut pending = String::new();
    let mut pending_poll = String::new();
    for p in &g.paths {
        let s = static_name(&p.nutrient);
        let (pat, slot) = to_slot(p);
        match p.cfg.shape {
            Shape::Broadcast => {
                let _ = writeln!(
                    release,
                    "            {pat} => {{\n                {s}.immediate_publisher().publish_immediate({slot});\n                Release(Pending::Done)\n            }}"
                );
                let _ = writeln!(
                    try_release,
                    "            {pat} => {{\n                {s}.immediate_publisher().publish_immediate({slot});\n                Ok(())\n            }}"
                );
            }
            Shape::Directed => {
                let v = format!("{}Send", p.nutrient);
                let _ = writeln!(
                    release,
                    "            {pat} => Release(Pending::{v}({s}.send({slot}))),"
                );
                let _ = writeln!(
                    try_release,
                    "            {pat} => {s}\n                .try_send({slot})\n                .map_err(|TrySendError::Full(s)| {}(s)),",
                    unslot_fn(p)
                );
                let _ = writeln!(
                    pending,
                    "    {v}(SendFuture<'static, M, {}, {}>),",
                    slot_ty(p),
                    p.cfg.cap
                );
                let _ = writeln!(
                    pending_poll,
                    "            Pending::{v}(f) => Pin::new(f).poll(cx),"
                );
            }
            Shape::State => {
                let _ = writeln!(
                    release,
                    "            {pat} => {{\n                {s}.sender().send({slot});\n                Release(Pending::Done)\n            }}"
                );
                let _ = writeln!(
                    try_release,
                    "            {pat} => {{\n                {s}.sender().send({slot});\n                Ok(())\n            }}"
                );
            }
        }
    }
    render_sap(o, &release, &try_release, &pending, &pending_poll);

    // depths()
    let _ = writeln!(
        o,
        "/// Queue depth per path — `(nutrient, queued, capacity)` — for diagnostics.\n\
         pub fn depths() -> [(&'static str, usize, usize); {}] {{\n    [",
        g.paths.len()
    );
    for p in &g.paths {
        let s = static_name(&p.nutrient);
        let len = match p.cfg.shape {
            Shape::State => format!("{s}.contains_value() as usize"),
            _ => format!("{s}.len()"),
        };
        let _ = writeln!(o, "        (\"{}\", {len}, {}),", p.nutrient, p.cfg.cap);
    }
    o.push_str("    ]\n}\n\n");

    // Taps, one module per node.
    o.push_str("// ── taps: one set per branch ───────────────────────────────────────────────\n");
    for node in g.nodes.iter().filter(|n| n.uses_taps || !n.taps.is_empty()) {
        let taps: Vec<&Path> = node.taps.iter().filter_map(|t| g.path(t)).collect();
        let mut fields = Vec::new();
        for p in &taps {
            let field = format!("{}_tap", snake(&p.nutrient));
            let s = static_name(&p.nutrient);
            match p.cfg.shape {
                Shape::Broadcast => fields.push((
                    field,
                    broadcast_ty("Subscriber", p),
                    format!("{s}.subscriber().unwrap_or_else(|_| miscounted())"),
                )),
                Shape::State => fields.push((
                    field,
                    format!(
                        "watch::Receiver<'static, M, {}, {}>",
                        slot_ty(p),
                        p.tappers.len()
                    ),
                    format!("{s}.receiver().unwrap_or_else(|| miscounted())"),
                )),
                Shape::Directed => {} // the Channel itself is the receiver
            }
        }
        let poll_of = |p: &Path| -> String {
            let field = format!("self.{}_tap", snake(&p.nutrient));
            let unslot = unslot_fn(p);
            match p.cfg.shape {
                Shape::Broadcast => format!(
                    "poll_broadcast(&mut {field}, WHO, \"{}\", &mut self.lagged, cx).map({unslot})",
                    p.nutrient
                ),
                Shape::Directed => format!(
                    "{}.poll_receive(cx).map({unslot})",
                    static_name(&p.nutrient)
                ),
                Shape::State => format!("poll_once(cx, {field}.changed()).map({unslot})"),
            }
        };
        let got = match taps.as_slice() {
            [] => None,
            [p] => Some(poll_of(p)),
            many => {
                // Round-robin: start each poll one past the path that last
                // delivered, so a busy path can't starve the rest.
                fields.push(("turn".into(), "usize".into(), "0".into()));
                let k = many.len();
                let mut arms = String::new();
                for (i, p) in many.iter().enumerate() {
                    let arm = if i + 1 == k {
                        "_".to_string()
                    } else {
                        i.to_string()
                    };
                    let _ = writeln!(arms, "                            {arm} => {},", poll_of(p));
                }
                Some(format!(
                    "'turns: {{\n\
                     \x20                   for i in 0..{k} {{\n\
                     \x20                       let k = (self.turn + i) % {k};\n\
                     \x20                       let got = match k {{\n{arms}\
                     \x20                       }};\n\
                     \x20                       if got.is_ready() {{\n\
                     \x20                           self.turn = k + 1;\n\
                     \x20                           break 'turns got;\n\
                     \x20                       }}\n\
                     \x20                   }}\n\
                     \x20                   Poll::Pending\n\
                     \x20               }}"
                ))
            }
        };
        let desc = if taps.is_empty() {
            "nothing yet".to_string()
        } else {
            taps.iter()
                .map(|p| p.nutrient.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        render_taps(o, &node.name, &desc, &fields, got.as_deref());
    }
}

/// One node's `pub mod <name> { pub struct Taps … }`. `fields` are
/// (name, type, initializer); `got` is an expression that polls the node's
/// paths once, as `Poll<Nutrient>` (None: the node taps nothing yet).
fn render_taps(
    o: &mut String,
    name: &str,
    desc: &str,
    fields: &[(String, String, String)],
    got: Option<&str>,
) {
    let _ = writeln!(
        o,
        "\n/// What `{name}` taps: {desc}.\npub mod {name} {{\n    use super::*;\n\n    const WHO: &str = \"{name}\";\n\n    pub struct Taps {{\n        lagged: u64,\n        streak: u8,"
    );
    for (f, ty, _) in fields {
        let _ = writeln!(o, "        {f}: {ty},");
    }
    o.push_str(
        "    }\n\n    impl Taps {\n\
         \x20       /// Claim this branch's taps. Call once, at startup.\n\
         \x20       pub fn new() -> Self {\n            Self {\n                lagged: 0,\n                streak: 0,\n",
    );
    for (f, _, init) in fields {
        let _ = writeln!(o, "                {f}: {init},");
    }
    o.push_str("            }\n        }\n\n");
    o.push_str(
        "        /// Nutrients lost on broadcast paths because this branch fell behind.\n\
         \x20       pub fn lagged(&self) -> u64 {\n            self.lagged\n        }\n\n",
    );
    let Some(got) = got else {
        o.push_str(
            "        /// Wait for the next tapped nutrient — never, until this branch taps one.\n\
             \x20       pub async fn next(&mut self) -> Nutrient {\n\
             \x20           core::future::pending().await\n        }\n    }\n}\n",
        );
        return;
    };
    let _ = writeln!(
        o,
        "        /// Wait for the next nutrient this branch taps. After a burst of\n\
         \x20       /// `BURST` back-to-back nutrients it yields to the executor once, so a\n\
         \x20       /// flood (or a branch feeding itself) can't starve the other branches.\n\
         \x20       pub async fn next(&mut self) -> Nutrient {{\n\
         \x20           poll_fn(|cx| {{\n\
         \x20               if self.streak == BURST {{\n\
         \x20                   self.streak = 0;\n\
         \x20                   cx.waker().wake_by_ref();\n\
         \x20                   return Poll::Pending;\n\
         \x20               }}\n\
         \x20               let got = {got};\n\
         \x20               self.streak = if got.is_ready() {{ self.streak + 1 }} else {{ 0 }};\n\
         \x20               got\n\
         \x20           }})\n\
         \x20           .await\n        }}\n    }}\n}}"
    );
}

fn render_trunk(g: &Graph, o: &mut String) {
    let cap = g.trunk_cap();
    let subs = g.trunk_tappers();
    let _ = writeln!(
        o,
        "/// The one shared bus: every nutrient, {cap} queued, {subs} tapping branches.\n\
         static TRUNK_BUS: PubSubChannel<M, Nutrient, {cap}, {subs}, 0> = PubSubChannel::new();"
    );
    render_sap(
        o,
        "            _ => {\n                TRUNK_BUS.immediate_publisher().publish_immediate(n);\n                Release(Pending::Done)\n            }\n",
        "            _ => {\n                TRUNK_BUS.immediate_publisher().publish_immediate(n);\n                Ok(())\n            }\n",
        "",
        "",
    );
    o.push_str(
        "/// Queue depth per path — `(nutrient, queued, capacity)` — for diagnostics.\n\
         pub fn depths() -> [(&'static str, usize, usize); 1] {\n",
    );
    let _ = writeln!(o, "    [(\"trunk\", TRUNK_BUS.len(), {cap})]\n}}\n");

    o.push_str("// ── taps: one set per branch ───────────────────────────────────────────────\n");
    for node in g.nodes.iter().filter(|n| n.uses_taps || !n.taps.is_empty()) {
        if node.taps.is_empty() {
            render_taps(o, &node.name, "nothing yet", &[], None);
            continue;
        }
        let fields = [(
            "bus".to_string(),
            format!("Subscriber<'static, M, Nutrient, {cap}, {subs}, 0>"),
            "TRUNK_BUS.subscriber().unwrap_or_else(|_| miscounted())".to_string(),
        )];
        // The bus carries everything; skip what this node doesn't tap.
        let pats: Vec<String> = node
            .taps
            .iter()
            .filter_map(|t| g.path(t))
            .map(|p| pat(&p.variant))
            .collect();
        let got = format!(
            "loop {{\n\
             \x20                   match poll_broadcast(&mut self.bus, WHO, \"trunk\", &mut self.lagged, cx) {{\n\
             \x20                       Poll::Ready(n @ ({})) => break Poll::Ready(n),\n\
             \x20                       Poll::Ready(_) => continue,\n\
             \x20                       Poll::Pending => break Poll::Pending,\n\
             \x20                   }}\n\
             \x20               }}",
            pats.join(" | ")
        );
        render_taps(o, &node.name, &node.taps.join(", "), &fields, Some(&got));
    }
}

fn render_sap(o: &mut String, release: &str, try_release: &str, pending: &str, poll: &str) {
    let _ = write!(
        o,
        "
/// The release side of the sap: zero-sized and `Copy`, so hand one to every
/// branch that feeds nutrients (`trunk.sap()`).
#[derive(Clone, Copy)]
pub struct Sap;

impl Sap {{
    /// Release a nutrient onto its path; `.await` the result. Broadcast and
    /// state paths take it at once and never wait (a broadcast tapper that falls
    /// behind loses its oldest); a directed path waits while it's full — see
    /// `try_release` for a version that doesn't.
    pub fn release(&self, n: Nutrient) -> Release {{
        match n {{
{release}        }}
    }}

    /// Like `release`, but never waits: a full directed path hands `n` back.
    pub fn try_release(&self, n: Nutrient) -> Result<(), Nutrient> {{
        match n {{
{try_release}        }}
    }}
}}

/// What `sap.release(..)` returns: already done for a broadcast or state path,
/// or a directed path's pending send. It holds only that path's slot — never a
/// whole `Nutrient` — so a task awaiting a release stays small.
#[must_use = \"a directed release only happens once awaited\"]
pub struct Release(Pending);

enum Pending {{
    Done,
{pending}}}

impl Future for Release {{
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {{
        match &mut self.get_mut().0 {{
            Pending::Done => {{
                let _ = cx;
                Poll::Ready(())
            }}
{poll}        }}
    }}
}}

"
    );
}

fn render_common(g: &Graph, o: &mut String) {
    o.push_str(
        "
// ── plumbing ───────────────────────────────────────────────────────────────

/// A path ran out of tapper slots. bonsai counts them from the wiring, so this
/// only happens after a hand edit it hasn't seen. (A plain panic, not an
/// `expect`, so no error-formatting code is linked in.)
fn miscounted() -> ! {
    panic!(\"sap: more taps than bonsai counted — run `bonsai sync`\")
}

/// Back-to-back nutrients a branch takes before `next()` yields to the executor.
const BURST: u8 = 32;

/// Called when branch `who` fell behind on a broadcast path and lost `missed`
/// nutrients (they're also counted in its `taps.lagged()`).
fn note_lag(who: &'static str, path: &'static str, missed: u64) {
",
    );
    match g.logger {
        Logger::Defmt => o.push_str(
            "    defmt::debug!(\"sap: {=str} lagged on {=str}, {=u64} lost\", who, path, missed);\n",
        ),
        Logger::Std => o.push_str(
            "    // Opt-in (a flood can lag thousands of times): run with BONSAI_SAP_DEBUG set.\n\
             \x20   static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();\n\
             \x20   if *ON.get_or_init(|| std::env::var_os(\"BONSAI_SAP_DEBUG\").is_some()) {\n\
             \x20       eprintln!(\"sap: {who} lagged on {path}, {missed} lost\");\n    }\n",
        ),
    }
    o.push_str(
        "}

/// Poll a broadcast subscriber, counting (and skipping past) any lag.
fn poll_broadcast<T: Clone, const CAP: usize, const SUBS: usize, const PUBS: usize>(
    sub: &mut Subscriber<'static, M, T, CAP, SUBS, PUBS>,
    who: &'static str,
    path: &'static str,
    lagged: &mut u64,
    cx: &mut Context<'_>,
) -> Poll<T> {
    loop {
        match pin!(sub.next_message()).poll(cx) {
            Poll::Ready(WaitResult::Message(n)) => return Poll::Ready(n),
            Poll::Ready(WaitResult::Lagged(missed)) => {
                *lagged += missed;
                note_lag(who, path, missed);
            }
            Poll::Pending => return Poll::Pending,
        }
    }
}

/// Poll a fresh future once. The receivers keep their state outside the
/// future, so dropping it after `Pending` loses nothing.
fn poll_once<F: Future>(cx: &mut Context<'_>, fut: F) -> Poll<F::Output> {
    pin!(fut).poll(cx)
}
",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[Variant]) -> Vec<&str> {
        v.iter().map(|v| v.name.as_str()).collect()
    }

    // Multi-line struct variants, tuple variants, attributes and docs all parse;
    // a field line is never mistaken for a variant.
    #[test]
    fn parse_variants_handles_every_shape() {
        let trunk = "pub enum Nutrient {\n    /// doc\n    Beat,\n    Target { pos: i32 },\n    Frame {\n        buf: heapless::Vec<u8, 64>,\n        len: usize,\n    },\n    #[allow(dead_code)]\n    Raw(u8, [u16; 4]),\n    // bonsai:nutrient\n}\nfn f() {}\n";
        let v = parse_variants(trunk);
        assert_eq!(names(&v), vec!["Beat", "Target", "Frame", "Raw"]);
        assert!(v[0].fields.is_empty());
        assert_eq!(v[1].fields, vec!["i32"]);
        assert_eq!(v[2].fields, vec!["heapless::Vec<u8, 64>", "usize"]);
        assert_eq!(v[3].fields, vec!["u8", "[u16; 4]"]);
    }

    #[test]
    fn type_sizes() {
        assert_eq!(type_size("u8", 4), Some((1, 1)));
        assert_eq!(type_size("[u8; 280]", 4), Some((280, 1)));
        assert_eq!(type_size("(u8, u32)", 4), Some((8, 4)));
        assert_eq!(type_size("heapless::Vec<u8, 64>", 4), Some((68, 4)));
        assert_eq!(type_size("heapless::String<32>", 4), Some((36, 4)));
        assert_eq!(type_size("Box<[u8; 4096]>", 4), Some((4, 4)));
        assert_eq!(type_size("&'static str", 4), Some((8, 4)));
        assert_eq!(type_size("Option<u16>", 4), Some((4, 2)));
        assert_eq!(type_size("MyThing", 4), None);
    }

    #[test]
    fn enum_size_is_largest_variant_plus_tag() {
        let v = parse_variants(
            "pub enum Nutrient {\n    Beat,\n    Frame { buf: [u8; 280], len: u16 },\n}\n",
        );
        assert_eq!(enum_size(&v, 4), Some(284));
        let unknown = parse_variants("pub enum Nutrient {\n    Beat,\n    X { t: Thing },\n}\n");
        assert_eq!(enum_size(&unknown, 4), None);
    }

    #[test]
    fn snake_case_names() {
        assert_eq!(snake("Beat"), "beat");
        assert_eq!(snake("LinkUp"), "link_up");
        assert_eq!(snake("GPSFix"), "gps_fix");
        assert_eq!(snake("UartRX"), "uart_rx");
        assert_eq!(snake("Gps2Fix"), "gps2_fix");
    }

    #[test]
    fn config_parses_and_defaults() {
        let cfg = Config::parse(
            "[sap]\nlayout = \"paths\"\n[nutrients.Rx]\nshape = \"directed\"\ncap = 64\n[nutrients.Armed]\nshape = \"state\"\n",
        )
        .unwrap();
        assert_eq!(cfg.layout, Layout::Paths);
        assert_eq!(cfg.path("Rx"), PathCfg::new(Shape::Directed, Some(64)));
        assert_eq!(cfg.path("Armed").cap, 1);
        assert_eq!(cfg.path("Unlisted"), PathCfg::default());
        assert!(Config::parse("[nutrients.X]\nshape = \"fast\"\n").is_err());
        assert!(Config::parse("[nutrients.X]\ncap = 0\n").is_err());
        assert!(Config::parse("[sap]\nlayout = \"tree\"\n").is_err());
    }

    // set_path keeps the file's comments and renders dotted tables; state drops cap.
    #[test]
    fn set_and_remove_path_round_trip() {
        let src = "# keep me\n[sap]\nlayout = \"paths\"\n";
        let out = set_path(src, "Rx", PathCfg::new(Shape::Directed, Some(64))).unwrap();
        assert!(out.starts_with("# keep me\n"), "{out}");
        assert!(
            out.contains("[nutrients.Rx]\nshape = \"directed\"\ncap = 64\n"),
            "{out}"
        );
        let out = set_path(&out, "Rx", PathCfg::new(Shape::State, None)).unwrap();
        assert!(!out.contains("cap"), "{out}");
        let out = remove_path(&out, "Rx").unwrap();
        assert!(!out.contains("Rx"), "{out}");
        assert!(Config::parse(&out).is_ok());
    }

    fn node(name: &str, taps: &[&str], releases: &[&str]) -> Node {
        let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        Node {
            name: name.into(),
            taps: v(taps),
            releases: v(releases),
            awaited: v(releases),
            uses_taps: !taps.is_empty(),
        }
    }

    // Unit, struct and tuple variants, so every slot shape gets rendered.
    fn graph(cfg: &str, nodes: Vec<Node>) -> Graph {
        let variants = parse_variants(
            "pub enum Nutrient {\n    Beat,\n    Cmd { code: u8, arg: i32 },\n    Ack,\n    Armed(bool),\n}\n",
        );
        Graph::build(&variants, &Config::parse(cfg).unwrap(), nodes, Logger::Std)
    }

    const SHAPED: &str = "[nutrients.Cmd]\nshape = \"directed\"\ncap = 8\n[nutrients.Ack]\nshape = \"directed\"\n[nutrients.Armed]\nshape = \"state\"\n";

    // SUBS / Watch receivers come from the graph, not a hand-tuned constant.
    #[test]
    fn render_counts_tappers_from_the_graph() {
        let g = graph(
            SHAPED,
            vec![
                node("pulse", &["Beat"], &["Beat"]),
                node("ui", &["Beat", "Armed"], &[]),
                node("ctl", &["Cmd"], &["Armed"]),
            ],
        );
        let out = render(&g);
        // Each path carries only its own payload: () for a unit variant, a
        // generated slot struct otherwise.
        assert!(
            out.contains("static BEAT_PATH: PubSubChannel<M, (), 4, 2, 0>"),
            "{out}"
        );
        assert!(
            out.contains("static CMD_PATH: Channel<M, CmdSlot, 8>"),
            "{out}"
        );
        assert!(
            out.contains("static ARMED_PATH: Watch<M, ArmedSlot, 1>"),
            "{out}"
        );
        assert!(out.contains("pub mod ui {"), "{out}");
        assert!(
            out.contains("_ => poll_once(cx, self.armed_tap.changed()).map(armed_nutrient),"),
            "{out}"
        );
        assert!(!out.contains("pub mod nobody"), "{out}");
    }

    // Slots mirror each variant's fields, and releasing moves the payload onto
    // its path: a directed path's pending send holds only that path's slot.
    #[test]
    fn render_moves_payloads_through_slots() {
        let g = graph(SHAPED, vec![node("ctl", &["Cmd", "Armed"], &["Cmd"])]);
        let out = render(&g);
        for want in [
            "pub struct CmdSlot {\n    pub code: u8,\n    pub arg: i32,\n}",
            "fn cmd_nutrient(CmdSlot { code, arg }: CmdSlot) -> Nutrient {\n    Nutrient::Cmd { code, arg }\n}",
            "pub struct ArmedSlot(pub bool);",
            "fn armed_nutrient(ArmedSlot(f0): ArmedSlot) -> Nutrient {\n    Nutrient::Armed(f0)\n}",
            "fn beat_nutrient(_: ()) -> Nutrient {",
            "Nutrient::Cmd { code, arg } => Release(Pending::CmdSend(CMD_PATH.send(CmdSlot { code, arg }))),",
            "Nutrient::Armed(f0) => {\n                ARMED_PATH.sender().send(ArmedSlot(f0));",
            ".map_err(|TrySendError::Full(s)| cmd_nutrient(s)),",
            "    CmdSend(SendFuture<'static, M, CmdSlot, 8>),",
            "CMD_PATH.poll_receive(cx).map(cmd_nutrient)",
            "#[must_use",
        ] {
            assert!(out.contains(want), "missing {want:?} in:\n{out}");
        }
        // No payload anywhere is a whole Nutrient any more.
        assert!(!out.contains("M, Nutrient,"), "{out}");
        assert!(!out.contains(".expect("), "{out}");
    }

    // Queued payload: per path in the paths layout, whole-enum slots on one bus.
    #[test]
    fn sap_bytes_per_layout() {
        let g = graph(SHAPED, vec![]);
        // Beat 4×0 + Cmd 8×8 + Ack 8×0 + Armed 1×1
        assert_eq!(g.sap_bytes(4), Some(65));
        let g = graph("[sap]\nlayout = \"trunk\"\n", vec![]);
        // cap 4 × (8 B payload + tag, aligned to 4)
        assert_eq!(g.sap_bytes(4), Some(48));
    }

    #[test]
    fn trunk_layout_uses_one_bus_and_filters() {
        let g = graph(
            "[sap]\nlayout = \"trunk\"\n[nutrients.Cmd]\ncap = 16\n",
            vec![
                node("pulse", &["Beat"], &["Beat"]),
                node("ctl", &["Cmd", "Ack"], &[]),
            ],
        );
        let out = render(&g);
        assert!(
            out.contains("static TRUNK_BUS: PubSubChannel<M, Nutrient, 16, 2, 0>"),
            "{out}"
        );
        assert!(
            out.contains("Poll::Ready(n @ (Nutrient::Cmd { .. } | Nutrient::Ack))"),
            "{out}"
        );
        assert!(!out.contains("_PATH"), "{out}");
    }

    #[test]
    fn warns_on_self_deadlock_and_cycles() {
        let g = graph(SHAPED, vec![node("ctl", &["Cmd"], &["Cmd"])]);
        assert!(
            g.warnings()
                .iter()
                .any(|w| w.contains("`ctl` taps and releases directed `Cmd`"))
        );

        let g = graph(
            SHAPED,
            vec![node("a", &["Ack"], &["Cmd"]), node("b", &["Cmd"], &["Ack"])],
        );
        let w = g.warnings();
        assert!(w.iter().any(|w| w.contains("cycle a → b → a")), "{w:?}");

        // try_release (not awaited) breaks the deadlock (the feedback note stays).
        let mut ctl = node("ctl", &["Cmd"], &["Cmd"]);
        ctl.awaited.clear();
        let w = graph(SHAPED, vec![ctl]).warnings();
        assert!(w.iter().all(|w| !w.contains("deadlock")), "{w:?}");

        // Broadcast never waits, so a self-loop there can't deadlock — but it
        // can still feed itself.
        let w = graph("", vec![node("ctl", &["Cmd"], &["Cmd"])]).warnings();
        assert!(w.iter().all(|w| !w.contains("deadlock")), "{w:?}");
        assert!(w.iter().any(|w| w.contains("feeds itself")), "{w:?}");
    }

    #[test]
    fn warns_on_directed_fan_out() {
        let g = graph(
            SHAPED,
            vec![node("a", &["Cmd"], &[]), node("b", &["Cmd"], &[])],
        );
        assert!(g.warnings().iter().any(|w| w.contains("tapped by a and b")));
    }
}

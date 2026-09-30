//! The live view: numbered tabs (1 Graph · 2 Branches · 3 Edges · 4 Log ·
//! 5 System), switched with their number, Tab, or a click.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Gauge, Paragraph, Row, Table, TableState};
use ratatui::{DefaultTerminal, Frame};

use super::graph::{self, Canvas, Node};
use super::{Snapshot, Update, WINDOW_MS, avg_us, per_sec, source, took, uptime};

/// Log lines top keeps.
const KEEP: usize = 1000;
/// How long a branch that panicked stays red.
const RED_FOR: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tab {
    Graph,
    Branches,
    Edges,
    Log,
    System,
}

const TABS: [(Tab, &str); 5] = [
    (Tab::Graph, "Graph"),
    (Tab::Branches, "Branches"),
    (Tab::Edges, "Edges"),
    (Tab::Log, "Log"),
    (Tab::System, "System"),
];

/// The levels the Log tab can be limited to, from everything to errors only.
const LEVELS: [(&str, &[&str]); 4] = [
    ("all", &["ERROR", "WARN", "INFO", "DEBUG"]),
    ("info+", &["ERROR", "WARN", "INFO"]),
    ("warn+", &["ERROR", "WARN"]),
    ("errors", &["ERROR"]),
];

pub struct App {
    title: String,
    /// Earlier snapshots, oldest first, back to `WINDOW_MS` ago.
    history: VecDeque<Snapshot>,
    now: Option<Snapshot>,
    logs: VecDeque<String>,
    status: Option<String>,
    pub tab: Tab,
    /// A node: a branch, then the edges, in the tree's order.
    selected: usize,
    /// Only this branch's or edge's lines, on the Log tab.
    filter: Option<String>,
    /// Only lines containing this, on the Log tab.
    search: String,
    /// Typing a search (after `/`).
    typing: bool,
    /// Which of `LEVELS` the Log tab shows.
    level: usize,
    /// Lines scrolled back from the end of the log.
    scroll: usize,
    paused: bool,
    /// When each branch's panic count last went up.
    panicked: HashMap<String, (u64, Instant)>,
    /// Where the last frame put each tab title and each node, for clicks.
    hits: RefCell<Hits>,
}

#[derive(Default)]
struct Hits {
    tabs: Vec<(Tab, Rect)>,
    nodes: Vec<(usize, Rect)>,
}

impl App {
    pub fn new(title: String) -> Self {
        App {
            title,
            history: VecDeque::new(),
            now: None,
            logs: VecDeque::new(),
            status: None,
            tab: Tab::Graph,
            selected: 0,
            filter: None,
            search: String::new(),
            typing: false,
            level: 0,
            scroll: 0,
            paused: false,
            panicked: HashMap::new(),
            hits: RefCell::new(Hits::default()),
        }
    }

    pub fn update(&mut self, u: Update) {
        match u {
            Update::Snapshot(mut s) => {
                self.status = None;
                // A restarted tree starts its counts again.
                if self.now.as_ref().is_some_and(|n| s.uptime_ms < n.uptime_ms) {
                    self.now = None;
                    self.history.clear();
                    self.panicked.clear();
                }
                for b in &s.branches {
                    let seen = self.panicked.get(&b.name).map_or(0, |(n, _)| *n);
                    if b.panics > seen {
                        self.panicked
                            .insert(b.name.clone(), (b.panics, Instant::now()));
                    }
                }
                for line in s.logs.drain(..) {
                    if self.logs.len() == KEEP {
                        self.logs.pop_front();
                    }
                    self.logs.push_back(line);
                }
                if !self.paused {
                    self.history.extend(self.now.take());
                    // Keep the newest one at least WINDOW_MS older than `s`.
                    while self
                        .history
                        .get(1)
                        .is_some_and(|h| h.uptime_ms + WINDOW_MS <= s.uptime_ms)
                    {
                        self.history.pop_front();
                    }
                    self.now = Some(s);
                }
            }
            Update::Down(why) => self.status = Some(why),
        }
    }

    /// The snapshot rates are taken from: about `WINDOW_MS` ago.
    fn before(&self) -> Option<&Snapshot> {
        self.history.front()
    }

    /// Milliseconds between the two snapshots rates are taken from.
    fn ms(&self) -> u64 {
        match (self.before(), &self.now) {
            (Some(b), Some(n)) => n.uptime_ms.saturating_sub(b.uptime_ms),
            _ => 0,
        }
    }

    /// Every node: the branches, then the edges.
    fn nodes(&self) -> Vec<Node> {
        let Some(s) = &self.now else {
            return Vec::new();
        };
        let b = s.branches.iter().map(|b| Node {
            name: b.name.clone(),
            edge: false,
        });
        b.chain(s.edges.iter().map(|e| Node {
            name: e.name.clone(),
            edge: true,
        }))
        .collect()
    }

    fn selected_name(&self) -> Option<String> {
        self.nodes().get(self.selected).map(|n| n.name.clone())
    }

    /// Inputs a second into a branch, or packets in and out of an edge.
    fn rate(&self, name: &str) -> f64 {
        let (Some(now), ms) = (&self.now, self.ms()) else {
            return 0.0;
        };
        let before = self.before();
        if let Some(b) = now.branches.iter().find(|b| b.name == name) {
            let was = before
                .and_then(|x| x.branches.iter().find(|x| x.name == name))
                .map_or(b.inputs, |x| x.inputs);
            return per_sec(was, b.inputs, ms);
        }
        if let Some(e) = now.edges.iter().find(|e| e.name == name) {
            let was = before
                .and_then(|x| x.edges.iter().find(|x| x.name == name))
                .map_or(e.received + e.sent, |x| x.received + x.sent);
            return per_sec(was, e.received + e.sent, ms);
        }
        0.0
    }

    fn link_rate(&self, i: usize) -> f64 {
        let Some(now) = &self.now else { return 0.0 };
        let Some(w) = now.links.get(i) else {
            return 0.0;
        };
        let was = self
            .before()
            .and_then(|b| b.links.get(i))
            .map_or(w.count, |x| x.count);
        per_sec(was, w.count, self.ms())
    }

    /// A node's colour: red for a recent panic or an edge retrying, green
    /// when busy, yellow while an edge starts, grey when idle.
    fn color(&self, node: &Node) -> Color {
        let Some(now) = &self.now else {
            return Color::DarkGray;
        };
        if node.edge {
            return match now.edges.iter().find(|e| e.name == node.name) {
                Some(e) if e.state == "retrying" => Color::Red,
                Some(e) if e.state == "starting" => Color::Yellow,
                _ if self.rate(&node.name) > 0.0 => Color::Green,
                _ => Color::Gray,
            };
        }
        if self
            .panicked
            .get(&node.name)
            .is_some_and(|(_, at)| at.elapsed() < RED_FOR)
        {
            return Color::Red;
        }
        if self.rate(&node.name) > 0.0 {
            Color::Green
        } else {
            Color::DarkGray
        }
    }

    // -- drawing --------------------------------------------------------------

    pub fn draw(&self, frame: &mut Frame) {
        let [head, tabs, body, help] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        self.draw_header(frame, head);
        self.draw_tabs(frame, tabs);
        self.hits.borrow_mut().nodes.clear();
        match self.tab {
            Tab::Graph => self.draw_graph(frame, body),
            Tab::Branches => self.draw_branches(frame, body),
            Tab::Edges => self.draw_edges(frame, body),
            Tab::Log => self.draw_log(frame, body, self.filter.as_deref()),
            Tab::System => self.draw_system(frame, body),
        }
        let keys = match self.tab {
            _ if self.typing => " type to search the log  enter done  esc clear",
            Tab::Graph => " 1-5/tab/click switch  ←→↑↓ select  enter its log  p pause  q quit",
            Tab::Branches | Tab::Edges => {
                " 1-5/tab/click switch  ↑↓ select  enter its log  p pause  q quit"
            }
            Tab::Log => {
                " 1-5/tab switch  ↑↓/pgup/pgdn scroll  / search  l level  esc clear  q quit"
            }
            Tab::System => " 1-5/tab/click switch  p pause  q quit",
        };
        frame.render_widget(
            Paragraph::new(keys).style(Style::new().fg(Color::DarkGray)),
            help,
        );
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let mut header = vec![Span::styled(
            format!(" {} ", self.title),
            Style::new().add_modifier(Modifier::BOLD),
        )];
        match (&self.status, &self.now) {
            (Some(why), _) => header.push(Span::styled(
                format!(" {why}; retrying "),
                Style::new().fg(Color::Red),
            )),
            (None, None) => header.push(Span::styled(
                " connecting… ",
                Style::new().fg(Color::DarkGray),
            )),
            (None, Some(s)) => {
                let events = self.before().map_or(0, |b| b.events);
                header.push(Span::raw(format!(
                    " up {}  {:.0} events/s  slowest event {}  {} waiting",
                    uptime(s.uptime_ms),
                    per_sec(events, s.events, self.ms()),
                    took(s.max_event_us),
                    s.inbox
                )));
            }
        }
        if self.paused {
            header.push(Span::styled("  paused", Style::new().fg(Color::Yellow)));
        }
        frame.render_widget(Paragraph::new(Line::from(header)), area);
    }

    fn draw_tabs(&self, frame: &mut Frame, area: Rect) {
        let mut spans = vec![Span::raw(" ")];
        let mut hits = Vec::new();
        let mut x = area.x + 1;
        for (i, (tab, name)) in TABS.iter().enumerate() {
            let text = format!(" {} {name} ", i + 1);
            let width = text.chars().count() as u16;
            let style = if *tab == self.tab {
                Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD)
            } else {
                Style::new().fg(Color::Gray)
            };
            hits.push((*tab, Rect::new(x, area.y, width, 1)));
            spans.push(Span::styled(text, style));
            spans.push(Span::raw(" "));
            x += width + 1;
        }
        self.hits.borrow_mut().tabs = hits;
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn draw_graph(&self, frame: &mut Frame, area: Rect) {
        let block = Block::bordered().title(" graph ");
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let Some(now) = &self.now else { return };
        let nodes = self.nodes();
        if nodes.is_empty() {
            frame.render_widget(
                Paragraph::new(" no branches yet").style(Style::new().fg(Color::DarkGray)),
                inner,
            );
            return;
        }
        let links: Vec<(String, Vec<String>)> = now
            .links
            .iter()
            .map(|w| (w.from.clone(), w.to.clone()))
            .collect();
        let conns = graph::arrows(&nodes, &links);
        let back = graph::back_arrows(&nodes, &conns);
        let cols = graph::columns(&nodes, &conns);

        // The detail strip takes the bottom; the graph the rest.
        let [space, detail] =
            Layout::vertical([Constraint::Min(3), Constraint::Length(6)]).areas(inner);
        if now.sys.is_none() {
            // A tree from before bonsai top drew graphs reports no links.
            let hint = " this tree reports no links: run `bonsai sync` in it to see them";
            let at = Rect::new(
                space.x,
                space.y + space.height.saturating_sub(1),
                space.width,
                1,
            );
            frame.render_widget(
                Paragraph::new(hint).style(Style::new().fg(Color::DarkGray)),
                at,
            );
        }

        // Boxes: one column of them per layer, stacked top-down.
        let stat = |n: &Node| -> String {
            if n.edge {
                let state = now
                    .edges
                    .iter()
                    .find(|e| e.name == n.name)
                    .map_or("?", |e| e.state.as_str());
                format!("{state} {:.1}/s", self.rate(&n.name))
            } else {
                format!("{:.1}/s", self.rate(&n.name))
            }
        };
        let ncols = cols.iter().max().map_or(1, |c| c + 1);
        let mut col_width = vec![0u16; ncols];
        for (i, n) in nodes.iter().enumerate() {
            let w = n.name.chars().count().max(stat(n).chars().count()) as u16 + 4;
            col_width[cols[i]] = col_width[cols[i]].max(w);
        }
        const GAP: u16 = 18;
        let mut col_x = vec![0u16; ncols];
        for c in 1..ncols {
            col_x[c] = col_x[c - 1] + col_width[c - 1] + GAP;
        }
        // Links that skip a column run along lanes above the boxes, and each
        // row of labels sits above the boxes' first row: make room for both.
        let long = conns
            .iter()
            .enumerate()
            .filter(|(k, l)| !back[*k] && cols[l.to] > cols[l.from] + 1)
            .count() as u16;
        let top = long + 1;
        let mut boxes = vec![Rect::default(); nodes.len()];
        let mut filled = vec![top; ncols];
        for (i, _) in nodes.iter().enumerate() {
            let c = cols[i];
            boxes[i] = Rect::new(col_x[c], filled[c], col_width[c], 4);
            filled[c] += 5;
        }
        let height = filled.iter().max().copied().unwrap_or(0);

        // Lines, in graph coordinates.
        let mut canvas = Canvas::default();
        let mut labels: Vec<(i32, i32, String, bool)> = Vec::new();
        let mut arrows: Vec<(i32, i32, char, bool)> = Vec::new();
        // Each link gets its own vertical in the gap to its sender's right
        // (a link to several nodes shares one).
        let mut verticals: HashMap<(usize, usize), i32> = HashMap::new();
        let mut in_gap = vec![0i32; ncols];
        let mut long_lanes = 0i32;
        let mut back_lanes = 0i32;
        for (k, l) in conns.iter().enumerate() {
            let (a, b) = (boxes[l.from], boxes[l.to]);
            let rate = self.link_rate(l.link);
            let heat = if rate > 0.0 { 2 } else { 1 };
            let label = match now.links.get(l.link) {
                Some(w) if !w.label.is_empty() => format!("{} {rate:.1}/s", w.label),
                _ => format!("{rate:.1}/s"),
            };
            let ya = a.y as i32 + 1;
            let yb = b.y as i32 + 1;
            let right = (a.x + a.width) as i32;
            if !back[k] && cols[l.to] > cols[l.from] {
                let first = !verticals.contains_key(&(l.from, l.link));
                let gap = cols[l.from];
                let gap_x = (col_x[gap] + col_width[gap]) as i32;
                let vx = *verticals.entry((l.from, l.link)).or_insert_with(|| {
                    in_gap[gap] += 1;
                    gap_x + 1 + (in_gap[gap] - 1).min(4) * 2
                });
                canvas.horizontal(ya, right, vx, heat);
                if cols[l.to] == cols[l.from] + 1 {
                    // Next column: down or up in the gap, into the target.
                    canvas.vertical(vx, ya, yb, heat);
                    canvas.horizontal(yb, vx, b.x as i32 - 1, heat);
                } else {
                    // Further: up to a lane over the boxes, across, and down
                    // just before the target.
                    let lane = long_lanes;
                    long_lanes += 1;
                    let vx2 = b.x as i32 - 2 - (lane % 3);
                    canvas.vertical(vx, ya, lane, heat);
                    canvas.horizontal(lane, vx, vx2, heat);
                    canvas.vertical(vx2, lane, yb, heat);
                    canvas.horizontal(yb, vx2, b.x as i32 - 1, heat);
                }
                arrows.push((b.x as i32 - 1, yb, '▶', heat > 1));
                if first {
                    labels.push((vx + 1, ya - 1, label, heat > 1));
                }
            } else {
                // Back (or within a column): out of the bottom, along a lane
                // under everything, up into the target's bottom.
                let lane = height as i32 + back_lanes;
                back_lanes += 1;
                let xa = (a.x + a.width / 2) as i32;
                let xb = (b.x + b.width / 2) as i32 + 1;
                canvas.vertical(xa, (a.y + a.height) as i32, lane, heat);
                canvas.horizontal(lane, xa, xb, heat);
                canvas.vertical(xb, lane, (b.y + b.height) as i32, heat);
                arrows.push((xb, (b.y + b.height) as i32, '▲', heat > 1));
                labels.push((xa.min(xb) + 2, lane, format!(" {label} "), heat > 1));
            }
        }

        // Scroll so the selected node shows.
        let sel = boxes.get(self.selected).copied().unwrap_or_default();
        let off_x = (sel.x + sel.width).saturating_sub(space.width).min(sel.x) as i32;
        let off_y = (sel.y + sel.height).saturating_sub(space.height) as i32;
        let place = |x: i32, y: i32| -> Option<(u16, u16)> {
            let (x, y) = (x - off_x, y - off_y);
            (x >= 0 && y >= 0 && x < space.width as i32 && y < space.height as i32)
                .then(|| (space.x + x as u16, space.y + y as u16))
        };
        let buf = frame.buffer_mut();
        for ((x, y), c, heat) in canvas.cells() {
            if let Some((px, py)) = place(x, y) {
                let color = if heat > 1 {
                    Color::Cyan
                } else {
                    Color::DarkGray
                };
                buf[(px, py)].set_char(c).set_fg(color);
            }
        }
        for (x, y, c, busy) in arrows {
            if let Some((px, py)) = place(x, y) {
                let color = if busy { Color::Cyan } else { Color::DarkGray };
                buf[(px, py)].set_char(c).set_fg(color);
            }
        }
        for (x, y, text, busy) in labels {
            let style = Style::new().fg(if busy { Color::White } else { Color::DarkGray });
            for (i, ch) in text.chars().enumerate() {
                if let Some((px, py)) = place(x + i as i32, y) {
                    buf[(px, py)].set_char(ch).set_style(style);
                }
            }
        }
        let mut hits = Vec::new();
        for (i, n) in nodes.iter().enumerate() {
            let r = boxes[i];
            let color = self.color(n);
            let mut style = Style::new().fg(color);
            if i == self.selected {
                style = style.add_modifier(Modifier::BOLD | Modifier::REVERSED);
            }
            if let (Some((x0, y0)), Some((x1, y1))) = (
                place(r.x as i32, r.y as i32),
                place((r.x + r.width - 1) as i32, (r.y + r.height - 1) as i32),
            ) {
                let rect = Rect::new(x0, y0, x1 - x0 + 1, y1 - y0 + 1);
                draw_box(buf, rect, &n.name, &stat(n), n.edge, style, color);
                hits.push((i, rect));
            }
        }
        self.hits.borrow_mut().nodes = hits;

        // The detail strip: the selected node's numbers and latest lines.
        if let Some(name) = self.selected_name() {
            let mut lines = vec![Line::from(Span::styled(
                self.detail(&name),
                Style::new().add_modifier(Modifier::BOLD),
            ))];
            let recent: Vec<&String> = self
                .logs
                .iter()
                .filter(|l| source(l) == Some(name.as_str()))
                .collect();
            for l in &recent[recent.len().saturating_sub(4)..] {
                lines.push(log_line(l));
            }
            frame.render_widget(
                Paragraph::new(lines).block(Block::new().borders(ratatui::widgets::Borders::TOP)),
                detail,
            );
        }
    }

    /// One line about a node, for the graph's detail strip.
    fn detail(&self, name: &str) -> String {
        let Some(now) = &self.now else {
            return String::new();
        };
        if let Some(b) = now.branches.iter().find(|b| b.name == name) {
            let s = if b.panics == 1 { "" } else { "s" };
            return format!(
                " {name}: {:.1} inputs/s, avg {} µs, max {} µs, {} panic{s}",
                self.rate(name),
                avg_us(b),
                b.max_us,
                b.panics
            );
        }
        if let Some(e) = now.edges.iter().find(|e| e.name == name) {
            let mut s = format!(
                " {name}: {}, {} in, {} out, {} dropped, {} lost, {} restarts",
                e.state,
                e.received,
                e.sent,
                e.dropped,
                e.lost(),
                e.restarts
            );
            if !e.error.is_empty() {
                s += &format!(" (last: {})", e.error);
            }
            return s;
        }
        String::new()
    }

    fn draw_branches(&self, frame: &mut Frame, area: Rect) {
        let Some(s) = &self.now else { return };
        let ms = self.ms();
        let rows = s.branches.iter().map(|b| {
            let before = self
                .before()
                .and_then(|x| x.branches.iter().find(|x| x.name == b.name));
            let (inputs, sent) = before.map_or((b.inputs, b.sent), |x| (x.inputs, x.sent));
            let panics = if b.panics > 0 {
                Span::styled(b.panics.to_string(), Style::new().fg(Color::Red))
            } else {
                Span::raw("0")
            };
            Row::new(vec![
                Line::from(b.name.clone()),
                Line::from(format!("{:.1}", per_sec(inputs, b.inputs, ms))).right_aligned(),
                Line::from(format!("{:.1}", per_sec(sent, b.sent, ms))).right_aligned(),
                Line::from(avg_us(b).to_string()).right_aligned(),
                Line::from(b.max_us.to_string()).right_aligned(),
                Line::from(panics).right_aligned(),
            ])
        });
        let widths = [
            Constraint::Min(16),
            Constraint::Length(9),
            Constraint::Length(9),
            Constraint::Length(9),
            Constraint::Length(9),
            Constraint::Length(7),
        ];
        let heads = ["branch", "inputs/s", "sent/s", "avg µs", "max µs", "panics"];
        let table = Table::new(rows, widths)
            .header(header_row(&heads, &["branch"]))
            .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
            .block(Block::bordered().title(" branches "));
        let n = s.branches.len();
        let mut state =
            TableState::default().with_selected((self.selected < n).then_some(self.selected));
        frame.render_stateful_widget(table, area, &mut state);
    }

    fn draw_edges(&self, frame: &mut Frame, area: Rect) {
        let Some(s) = &self.now else { return };
        let ms = self.ms();
        let rows = s.edges.iter().map(|e| {
            let before = self
                .before()
                .and_then(|x| x.edges.iter().find(|x| x.name == e.name));
            let (rx, tx) = before.map_or((e.received, e.sent), |x| (x.received, x.sent));
            let color = match e.state.as_str() {
                "up" => Color::Green,
                "retrying" => Color::Red,
                _ => Color::Yellow,
            };
            Row::new(vec![
                Line::from(e.name.clone()),
                Line::from(Span::styled(e.state.clone(), Style::new().fg(color))),
                Line::from(format!("{:.1}", per_sec(rx, e.received, ms))).right_aligned(),
                Line::from(format!("{:.1}", per_sec(tx, e.sent, ms))).right_aligned(),
                Line::from(e.dropped.to_string()).right_aligned(),
                Line::from(e.lost().to_string()).right_aligned(),
                Line::from(e.restarts.to_string()).right_aligned(),
                Line::from(e.error.clone()),
            ])
        });
        let widths = [
            Constraint::Length(16),
            Constraint::Length(9),
            Constraint::Length(9),
            Constraint::Length(9),
            Constraint::Length(9),
            Constraint::Length(9),
            Constraint::Length(9),
            Constraint::Min(10),
        ];
        let heads = [
            "edge",
            "state",
            "in/s",
            "out/s",
            "dropped",
            "lost",
            "restarts",
            "last error",
        ];
        let table = Table::new(rows, widths)
            .header(header_row(&heads, &["edge", "state", "last error"]))
            .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
            .block(Block::bordered().title(" edges "));
        let b = s.branches.len();
        let mut state = TableState::default()
            .with_selected(self.selected.checked_sub(b).filter(|&i| i < s.edges.len()));
        frame.render_stateful_widget(table, area, &mut state);
    }

    /// The log lines the Log tab shows, oldest first.
    fn shown_logs(&self, source_filter: Option<&str>) -> Vec<&String> {
        let levels = LEVELS[self.level].1;
        self.logs
            .iter()
            .filter(|l| source_filter.is_none_or(|f| source(l) == Some(f)))
            .filter(|l| {
                let label = l.get(13..19).map(str::trim).unwrap_or("");
                levels.contains(&label)
            })
            .filter(|l| self.search.is_empty() || l.contains(&self.search))
            .collect()
    }

    fn draw_log(&self, frame: &mut Frame, area: Rect, source_filter: Option<&str>) {
        let fits = area.height.saturating_sub(2) as usize;
        let shown = self.shown_logs(source_filter);
        let end = shown.len().saturating_sub(self.scroll);
        let lines: Vec<Line> = shown[end.saturating_sub(fits)..end]
            .iter()
            .map(|l| log_line(l))
            .collect();
        let mut title = String::from(" log");
        if let Some(f) = source_filter {
            title += &format!(": {f}");
        }
        if self.level > 0 {
            title += &format!(" ({})", LEVELS[self.level].0);
        }
        if !self.search.is_empty() || self.typing {
            title += &format!(" /{}", self.search);
            if self.typing {
                title.push('_');
            }
        }
        if self.scroll > 0 {
            title += &format!(" (↑{})", self.scroll);
        }
        title.push(' ');
        frame.render_widget(
            Paragraph::new(lines).block(Block::bordered().title(title)),
            area,
        );
    }

    fn draw_system(&self, frame: &mut Frame, area: Rect) {
        let block = Block::bordered().title(" system ");
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let Some(now) = &self.now else { return };
        let Some(sys) = &now.sys else {
            frame.render_widget(
                Paragraph::new(" this tree doesn't report its system (run `bonsai sync` in it)")
                    .style(Style::new().fg(Color::DarkGray)),
                inner,
            );
            return;
        };
        let cpu = match self.before().and_then(|b| b.sys.as_ref()) {
            Some(was) if self.ms() > 0 => {
                sys.cpu_ms.saturating_sub(was.cpu_ms) as f64 / self.ms() as f64
            }
            _ => 0.0,
        };
        let used_kb = sys.mem_total_kb.saturating_sub(sys.mem_available_kb);
        let mem = if sys.mem_total_kb > 0 {
            used_kb as f64 / sys.mem_total_kb as f64
        } else {
            0.0
        };
        let tree_mem = if sys.mem_total_kb > 0 {
            sys.rss_kb as f64 / sys.mem_total_kb as f64
        } else {
            0.0
        };
        let rows = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Min(1),
        ])
        .split(inner);
        let gauge = |label: String, ratio: f64, color: Color| {
            Gauge::default()
                .label(label)
                .ratio(ratio.clamp(0.0, 1.0))
                .gauge_style(Style::new().fg(color))
        };
        frame.render_widget(
            gauge(
                format!("tree CPU {:.1}% of one core", cpu * 100.0),
                cpu,
                Color::Cyan,
            ),
            rows[0],
        );
        frame.render_widget(
            gauge(
                format!(
                    "tree memory {:.1} MB ({:.1}% of the computer's)",
                    sys.rss_kb as f64 / 1024.0,
                    tree_mem * 100.0
                ),
                tree_mem,
                Color::Magenta,
            ),
            rows[1],
        );
        frame.render_widget(
            gauge(
                format!(
                    "computer memory {:.0} of {:.0} MB used",
                    used_kb as f64 / 1024.0,
                    sys.mem_total_kb as f64 / 1024.0
                ),
                mem,
                Color::Yellow,
            ),
            rows[2],
        );
        let text = vec![
            Line::from(format!(" threads   {}", sys.threads)),
            Line::from(format!(" load      {:.2} (1 min)", sys.load as f64 / 100.0)),
            Line::from(format!(" uptime    {}", uptime(now.uptime_ms))),
            Line::from(format!(" events    {}", now.events)),
        ];
        frame.render_widget(Paragraph::new(text), rows[3]);
    }

    // -- input ----------------------------------------------------------------

    /// Handle a key; false to quit.
    pub fn key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        if self.typing {
            match code {
                KeyCode::Enter => self.typing = false,
                KeyCode::Esc => {
                    self.typing = false;
                    self.search.clear();
                }
                KeyCode::Backspace => {
                    self.search.pop();
                }
                KeyCode::Char(c) => self.search.push(c),
                _ => {}
            }
            return true;
        }
        let count = self.nodes().len();
        match code {
            KeyCode::Char('q') => return false,
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => return false,
            KeyCode::Char(c @ '1'..='5') => self.tab = TABS[c as usize - '1' as usize].0,
            KeyCode::Tab => self.tab = next_tab(self.tab, 1),
            KeyCode::BackTab => self.tab = next_tab(self.tab, TABS.len() - 1),
            KeyCode::Char('p') => self.paused = !self.paused,
            KeyCode::Esc if self.filter.is_some() || !self.search.is_empty() => {
                self.filter = None;
                self.search.clear();
            }
            KeyCode::Esc => return false,
            KeyCode::Char('/') if self.tab == Tab::Log => self.typing = true,
            KeyCode::Char('l') if self.tab == Tab::Log => self.level = (self.level + 1) % 4,
            KeyCode::Up | KeyCode::Char('k') if self.tab == Tab::Log => self.scroll += 1,
            KeyCode::Down | KeyCode::Char('j') if self.tab == Tab::Log => {
                self.scroll = self.scroll.saturating_sub(1)
            }
            KeyCode::PageUp if self.tab == Tab::Log => self.scroll += 10,
            KeyCode::PageDown if self.tab == Tab::Log => {
                self.scroll = self.scroll.saturating_sub(10)
            }
            KeyCode::End if self.tab == Tab::Log => self.scroll = 0,
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(count.saturating_sub(1))
            }
            KeyCode::Left | KeyCode::Char('h') => self.step_column(-1),
            KeyCode::Right | KeyCode::Char('l') => self.step_column(1),
            KeyCode::Enter => {
                self.filter = self.selected_name();
                self.scroll = 0;
                self.tab = Tab::Log;
            }
            _ => {}
        }
        true
    }

    /// On the graph, move to the nearest node in the next column over.
    fn step_column(&mut self, dir: i32) {
        let Some(now) = &self.now else { return };
        let nodes = self.nodes();
        let links: Vec<(String, Vec<String>)> = now
            .links
            .iter()
            .map(|w| (w.from.clone(), w.to.clone()))
            .collect();
        let cols = graph::columns(&nodes, &graph::arrows(&nodes, &links));
        let Some(&here) = cols.get(self.selected) else {
            return;
        };
        let want = here as i32 + dir;
        if want < 0 {
            return;
        }
        if let Some(i) = (0..nodes.len()).find(|&i| cols[i] as i32 == want) {
            self.selected = i;
        }
    }

    /// A left click: on a tab title, switch; on a node, select it.
    pub fn click(&mut self, x: u16, y: u16) {
        let at = Position::new(x, y);
        let hits = self.hits.borrow();
        if let Some((tab, _)) = hits.tabs.iter().find(|(_, r)| r.contains(at)) {
            let tab = *tab;
            drop(hits);
            self.tab = tab;
            return;
        }
        if let Some((i, _)) = hits.nodes.iter().find(|(_, r)| r.contains(at)) {
            let i = *i;
            drop(hits);
            self.selected = i;
        }
    }
}

fn next_tab(tab: Tab, by: usize) -> Tab {
    let i = TABS.iter().position(|(t, _)| *t == tab).unwrap_or(0);
    TABS[(i + by) % TABS.len()].0
}

fn header_row<'a>(heads: &[&'a str], left: &[&str]) -> Row<'a> {
    Row::new(heads.iter().map(|h| {
        if left.contains(h) {
            Line::from(*h)
        } else {
            Line::from(*h).right_aligned()
        }
    }))
    .style(Style::new().add_modifier(Modifier::BOLD))
}

/// A node's box: rounded for an edge (the outside world), square for a
/// branch; its name, and a line of numbers.
fn draw_box(
    buf: &mut Buffer,
    r: Rect,
    name: &str,
    stat: &str,
    edge: bool,
    style: Style,
    color: Color,
) {
    let (tl, tr, bl, br) = if edge {
        ('╭', '╮', '╰', '╯')
    } else {
        ('┌', '┐', '└', '┘')
    };
    let border = Style::new().fg(color);
    for x in r.x..r.x + r.width {
        for y in r.y..r.y + r.height {
            buf[(x, y)].set_char(' ').set_style(Style::new());
        }
    }
    for x in r.x + 1..r.x + r.width - 1 {
        buf[(x, r.y)].set_char('─').set_style(border);
        buf[(x, r.y + r.height - 1)].set_char('─').set_style(border);
    }
    for y in r.y + 1..r.y + r.height - 1 {
        buf[(r.x, y)].set_char('│').set_style(border);
        buf[(r.x + r.width - 1, y)].set_char('│').set_style(border);
    }
    buf[(r.x, r.y)].set_char(tl).set_style(border);
    buf[(r.x + r.width - 1, r.y)].set_char(tr).set_style(border);
    buf[(r.x, r.y + r.height - 1)]
        .set_char(bl)
        .set_style(border);
    buf[(r.x + r.width - 1, r.y + r.height - 1)]
        .set_char(br)
        .set_style(border);
    let inside = r.width.saturating_sub(4) as usize;
    buf.set_stringn(r.x + 2, r.y + 1, name, inside, style);
    buf.set_stringn(r.x + 2, r.y + 2, stat, inside, Style::new().fg(Color::Gray));
}

/// A log line with its level coloured, as the tree shows it on a terminal.
pub fn log_line(l: &str) -> Line<'_> {
    let Some((time, rest)) = l.split_at_checked(13) else {
        return Line::from(l);
    };
    let Some((label, rest)) = rest.split_at_checked(6) else {
        return Line::from(l);
    };
    let color = match label.trim() {
        "ERROR" => Color::Red,
        "WARN" => Color::Yellow,
        "INFO" => Color::Green,
        _ => Color::Blue,
    };
    Line::from(vec![
        Span::styled(time, Style::new().fg(Color::DarkGray)),
        Span::styled(label, Style::new().fg(color)),
        Span::raw(rest),
    ])
}

pub fn run(mut term: DefaultTerminal, rx: mpsc::Receiver<Update>, title: String) -> io::Result<()> {
    execute!(io::stdout(), EnableMouseCapture)?;
    let result = (|| {
        let mut app = App::new(title);
        loop {
            while let Ok(u) = rx.try_recv() {
                app.update(u);
            }
            term.draw(|f| app.draw(f))?;
            if !event::poll(Duration::from_millis(100))? {
                continue;
            }
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if !app.key(key.code, key.modifiers) {
                        return Ok(());
                    }
                }
                Event::Mouse(m) if m.kind == MouseEventKind::Down(MouseButton::Left) => {
                    app.click(m.column, m.row);
                }
                _ => {}
            }
        }
    })();
    execute!(io::stdout(), DisableMouseCapture)?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::top::{Branch, Link};

    fn snapshot(ms: u64) -> Update {
        Update::Snapshot(Snapshot {
            uptime_ms: ms,
            ..Default::default()
        })
    }

    #[test]
    fn rates_are_taken_over_about_two_seconds() {
        let mut app = App::new("t".into());
        for ms in (0..=3000).step_by(500) {
            app.update(snapshot(ms));
        }
        assert_eq!(app.before().map(|b| b.uptime_ms), Some(1000));
        assert_eq!(app.ms(), 2000);
        // A restarted tree starts over.
        app.update(snapshot(500));
        assert_eq!(app.ms(), 0);
    }

    #[test]
    fn tabs_switch_by_number_tab_and_back() {
        let mut app = App::new("t".into());
        assert_eq!(app.tab, Tab::Graph);
        app.key(KeyCode::Char('4'), KeyModifiers::NONE);
        assert_eq!(app.tab, Tab::Log);
        app.key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.tab, Tab::System);
        app.key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.tab, Tab::Graph);
        app.key(KeyCode::BackTab, KeyModifiers::NONE);
        assert_eq!(app.tab, Tab::System);
        assert!(!app.key(KeyCode::Char('q'), KeyModifiers::NONE));
    }

    #[test]
    fn the_log_tab_filters_by_source_level_and_text() {
        let mut app = App::new("t".into());
        let mut s = Snapshot {
            uptime_ms: 1,
            branches: vec![Branch {
                name: "display".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        s.logs = vec![
            "12:00:00.000Z  INFO display: 26.5 °C".into(),
            "12:00:00.000Z  WARN display: too hot".into(),
            "12:00:00.000Z DEBUG sensor: raw 265".into(),
        ];
        app.update(Update::Snapshot(s));
        assert_eq!(app.shown_logs(None).len(), 3);
        assert_eq!(app.shown_logs(Some("display")).len(), 2);
        app.tab = Tab::Log;
        app.key(KeyCode::Char('l'), KeyModifiers::NONE); // info+
        assert_eq!(app.shown_logs(None).len(), 2);
        app.key(KeyCode::Char('l'), KeyModifiers::NONE); // warn+
        assert_eq!(app.shown_logs(None).len(), 1);
        app.level = 0;
        app.key(KeyCode::Char('/'), KeyModifiers::NONE);
        for c in "°C".chars() {
            app.key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        app.key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.shown_logs(None).len(), 1);
        // Esc clears it; Enter on a node shows that node's log.
        app.key(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.shown_logs(None).len(), 3);
        app.tab = Tab::Graph;
        app.key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            (app.tab, app.filter.as_deref()),
            (Tab::Log, Some("display"))
        );
    }

    #[test]
    fn a_recent_panic_turns_a_branch_red() {
        let mut app = App::new("t".into());
        let at = |ms: u64, panics: u64| {
            Update::Snapshot(Snapshot {
                uptime_ms: ms,
                branches: vec![Branch {
                    name: "display".into(),
                    panics,
                    ..Default::default()
                }],
                links: vec![Link::default()],
                ..Default::default()
            })
        };
        app.update(at(0, 0));
        let node = Node {
            name: "display".into(),
            edge: false,
        };
        assert_eq!(app.color(&node), Color::DarkGray);
        app.update(at(500, 1));
        assert_eq!(app.color(&node), Color::Red);
    }
}

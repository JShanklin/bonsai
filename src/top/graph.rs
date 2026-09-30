//! The node graph `bonsai top` draws: where each branch and edge goes, and
//! the lines between them. `layout` is pure; `Canvas` turns lines into
//! box-drawing characters that join where they meet.

use std::collections::HashMap;

/// A branch or an edge.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub name: String,
    pub edge: bool,
}

/// One arrow: from node to node, for wire `wire`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Link {
    pub from: usize,
    pub to: usize,
    pub wire: usize,
}

/// The arrows the wires make, one per receiver; wires naming a node that
/// isn't there are skipped.
pub fn links(nodes: &[Node], wires: &[(String, Vec<String>)]) -> Vec<Link> {
    let at = |name: &str| nodes.iter().position(|n| n.name == name);
    let mut out = Vec::new();
    for (i, (from, to)) in wires.iter().enumerate() {
        let Some(f) = at(from) else { continue };
        for t in to {
            if let Some(t) = at(t) {
                out.push(Link {
                    from: f,
                    to: t,
                    wire: i,
                });
            }
        }
    }
    out
}

/// Each node's column, left to right: a node goes one column right of the
/// furthest node that feeds it. Links that close a loop don't count (they're
/// drawn going back), and edges the tree only sends to go at least as far
/// right as the last branch.
pub fn columns(nodes: &[Node], links: &[Link]) -> Vec<usize> {
    let n = nodes.len();
    let back = back_links(nodes, links);
    // Longest path over the forward links, in an order where every node
    // comes after the nodes feeding it.
    let order = topo_order(n, links, &back);
    let mut col = vec![0; n];
    for &v in &order {
        for (i, l) in links.iter().enumerate() {
            if l.to == v && !back[i] && l.from != l.to {
                col[v] = col[v].max(col[l.from] + 1);
            }
        }
    }
    let sink = |v: usize| nodes[v].edge && !links.iter().any(|l| l.from == v);
    let last = (0..n).filter(|&v| !sink(v)).map(|v| col[v]).max();
    for (v, c) in col.iter_mut().enumerate() {
        if sink(v) && links.iter().any(|l| l.to == v) {
            *c = last.unwrap_or(0).max(*c);
        }
    }
    col
}

/// Which links close a loop: a depth-first walk from the edges that feed the
/// tree (then the nodes nothing feeds, then the rest) finds each as a link
/// back to a node still being walked.
pub fn back_links(nodes: &[Node], links: &[Link]) -> Vec<bool> {
    let n = nodes.len();
    let mut back = vec![false; links.len()];
    let mut state = vec![0u8; n]; // 0 new, 1 being walked, 2 done
    let fed = |v: usize| links.iter().any(|l| l.to == v && l.from != v);
    let feeds = |v: usize| links.iter().any(|l| l.from == v);
    let mut starts: Vec<usize> = (0..n).filter(|&v| nodes[v].edge && feeds(v)).collect();
    starts.extend((0..n).filter(|&v| !fed(v)));
    starts.extend(0..n);
    for s in starts {
        if state[s] != 0 {
            continue;
        }
        // (node, next link index to look at)
        let mut stack = vec![(s, 0usize)];
        state[s] = 1;
        while let Some(&mut (v, ref mut next)) = stack.last_mut() {
            if let Some((i, l)) = links
                .iter()
                .enumerate()
                .skip(*next)
                .find(|(_, l)| l.from == v)
            {
                *next = i + 1;
                match state[l.to] {
                    0 => {
                        state[l.to] = 1;
                        stack.push((l.to, 0));
                    }
                    1 => back[i] = true,
                    _ => {}
                }
            } else {
                state[v] = 2;
                stack.pop();
            }
        }
    }
    back
}

fn topo_order(n: usize, links: &[Link], back: &[bool]) -> Vec<usize> {
    let mut indegree = vec![0; n];
    for (i, l) in links.iter().enumerate() {
        if !back[i] && l.from != l.to {
            indegree[l.to] += 1;
        }
    }
    let mut ready: Vec<usize> = (0..n).filter(|&v| indegree[v] == 0).collect();
    let mut order = Vec::new();
    while !ready.is_empty() {
        let v = ready.remove(0);
        order.push(v);
        for (i, l) in links.iter().enumerate() {
            if l.from == v && !back[i] && l.from != l.to {
                indegree[l.to] -= 1;
                if indegree[l.to] == 0 {
                    ready.push(l.to);
                }
            }
        }
    }
    order
}

const UP: u8 = 1;
const DOWN: u8 = 2;
const LEFT: u8 = 4;
const RIGHT: u8 = 8;

/// Lines on a grid of cells; where they meet, the right junction is drawn.
#[derive(Default)]
pub struct Canvas {
    cells: HashMap<(i32, i32), u8>,
    /// How busy each cell's line is: the busiest wire through it wins.
    heat: HashMap<(i32, i32), u8>,
}

impl Canvas {
    fn mark(&mut self, x: i32, y: i32, dirs: u8, heat: u8) {
        *self.cells.entry((x, y)).or_default() |= dirs;
        let h = self.heat.entry((x, y)).or_default();
        *h = (*h).max(heat);
    }

    /// A horizontal line from x0 to x1 (either way round) on row y.
    pub fn horizontal(&mut self, y: i32, x0: i32, x1: i32, heat: u8) {
        let (a, b) = (x0.min(x1), x0.max(x1));
        for x in a..=b {
            let mut d = 0;
            if x > a {
                d |= LEFT;
            }
            if x < b {
                d |= RIGHT;
            }
            self.mark(x, y, d, heat);
        }
    }

    /// A vertical line from y0 to y1 (either way round) in column x.
    pub fn vertical(&mut self, x: i32, y0: i32, y1: i32, heat: u8) {
        let (a, b) = (y0.min(y1), y0.max(y1));
        for y in a..=b {
            let mut d = 0;
            if y > a {
                d |= UP;
            }
            if y < b {
                d |= DOWN;
            }
            self.mark(x, y, d, heat);
        }
    }

    /// Every cell a line passes through: its character and heat.
    pub fn cells(&self) -> impl Iterator<Item = ((i32, i32), char, u8)> + '_ {
        self.cells.iter().filter_map(|(&at, &d)| {
            let c = match d {
                0 => return None,
                d if d == LEFT || d == RIGHT || d == LEFT | RIGHT => '─',
                d if d == UP || d == DOWN || d == UP | DOWN => '│',
                d if d == DOWN | RIGHT => '┌',
                d if d == DOWN | LEFT => '┐',
                d if d == UP | RIGHT => '└',
                d if d == UP | LEFT => '┘',
                d if d == UP | DOWN | RIGHT => '├',
                d if d == UP | DOWN | LEFT => '┤',
                d if d == LEFT | RIGHT | DOWN => '┬',
                d if d == LEFT | RIGHT | UP => '┴',
                _ => '┼',
            };
            Some((at, c, self.heat.get(&at).copied().unwrap_or(0)))
        })
    }

    /// The canvas as text, for tests.
    #[cfg(test)]
    pub fn text(&self) -> String {
        let (mut w, mut h) = (0, 0);
        for &(x, y) in self.cells.keys() {
            w = w.max(x + 1);
            h = h.max(y + 1);
        }
        let mut rows = vec![vec![' '; w as usize]; h as usize];
        for ((x, y), c, _) in self.cells() {
            rows[y as usize][x as usize] = c;
        }
        rows.into_iter()
            .map(|r| r.into_iter().collect::<String>().trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nodes(names: &[(&str, bool)]) -> Vec<Node> {
        names
            .iter()
            .map(|(n, e)| Node {
                name: n.to_string(),
                edge: *e,
            })
            .collect()
    }

    fn wires(w: &[(&str, &[&str])]) -> Vec<(String, Vec<String>)> {
        w.iter()
            .map(|(f, t)| (f.to_string(), t.iter().map(|t| t.to_string()).collect()))
            .collect()
    }

    #[test]
    fn a_chain_and_a_fan_out_go_left_to_right() {
        let n = nodes(&[("sensor", false), ("watchdog", false), ("display", false)]);
        let w = wires(&[
            ("sensor", &["watchdog", "display"]),
            ("watchdog", &["display"]),
        ]);
        let l = links(&n, &w);
        assert_eq!(l.len(), 3);
        assert_eq!(columns(&n, &l), [0, 1, 2]);
    }

    #[test]
    fn a_loop_is_drawn_back_and_an_edge_that_feeds_goes_first() {
        // greenhouse: the uplink feeds the watchdog, which answers it.
        let n = nodes(&[
            ("sensor", false),
            ("watchdog", false),
            ("display", false),
            ("uplink", true),
        ]);
        let w = wires(&[
            ("sensor", &["watchdog", "display"]),
            ("watchdog", &["display"]),
            ("uplink", &["watchdog"]),
            ("watchdog", &["uplink"]),
        ]);
        let l = links(&n, &w);
        let back = back_links(&n, &l);
        // Only watchdog → uplink closes the loop.
        let closing: Vec<(usize, usize)> = l
            .iter()
            .zip(&back)
            .filter(|(_, b)| **b)
            .map(|(l, _)| (l.from, l.to))
            .collect();
        assert_eq!(closing, [(1, 3)]);
        assert_eq!(columns(&n, &l), [0, 1, 2, 0]);
    }

    #[test]
    fn edges_that_only_receive_go_last() {
        let n = nodes(&[
            ("gps", true),
            ("position", false),
            ("log", true),
            ("display", false),
        ]);
        let w = wires(&[("gps", &["position"]), ("position", &["log", "display"])]);
        assert_eq!(columns(&n, &links(&n, &w)), [0, 1, 2, 2]);
    }

    #[test]
    fn lines_meet_in_junctions() {
        let mut c = Canvas::default();
        c.horizontal(1, 0, 4, 1);
        c.vertical(4, 1, 3, 1);
        c.horizontal(3, 4, 8, 1);
        c.horizontal(1, 4, 8, 1);
        assert_eq!(c.text(), "\n────┬────\n    │\n    └────");
    }
}

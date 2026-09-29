//! Column and adaptive radial layouts for the Folders and Subjects graph.
//!
//! The root is centred between two columns of children. Each node on the open
//! path grows one more column outward on its side. Label slots are measured for
//! every role a node may take, so selecting a sibling does not move its column.

use std::sync::Arc;

use gpui::{Bounds, Point, point, size};

use super::tree::GraphTree;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphLayoutKind {
    Columns,
    AdaptiveRadial,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LabelRole {
    Root,
    Path,
    Selection,
    Fan,
    Context,
}

pub trait LabelMetrics {
    fn text_width(&self, text: &str, role: LabelRole) -> f32;
    fn count_width(&self, count: usize, role: LabelRole) -> f32;
    fn line_height(&self, role: LabelRole) -> f32;
}

#[derive(Clone, Copy, Debug)]
pub struct RoleWidths {
    pub root: f32,
    pub path: f32,
    pub selection: f32,
    pub child: f32,
}

impl RoleWidths {
    fn of(&self, role: LabelRole) -> f32 {
        match role {
            LabelRole::Root => self.root,
            LabelRole::Path => self.path,
            LabelRole::Selection => self.selection,
            LabelRole::Fan | LabelRole::Context => self.child,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct LayoutParams {
    pub column_gap: f32,
    pub row_gap: f32,
    pub root_radius: f32,
    pub fan_angle_step: f32,
    pub fan_max_angle: f32,
    pub radial_gap: f32,
    pub radial_step: f32,
    pub path_parent_gap: f32,
    pub pad_x: f32,
    pub pad_y: f32,
    pub min_height: f32,
    pub hit_min_height: f32,
    pub nearest: f32,
    pub min_width: RoleWidths,
    pub max_width: RoleWidths,
    pub column_max_width: RoleWidths,
}

impl LayoutParams {
    pub fn desktop() -> Self {
        Self {
            column_gap: 72.0, row_gap: 10.0,
            root_radius: 270.0, fan_angle_step: 28.0, fan_max_angle: 160.0,
            radial_gap: 22.0, radial_step: 12.0, path_parent_gap: 40.0,
            pad_x: 20.0, pad_y: 16.0,
            min_height: 42.0, hit_min_height: 26.0, nearest: 12.0,
            min_width: RoleWidths { root: 116.0, path: 78.0, selection: 92.0, child: 68.0 },
            max_width: RoleWidths { root: 240.0, path: 205.0, selection: 235.0, child: 198.0 },
            column_max_width: RoleWidths { root: 320.0, path: 288.0, selection: 310.0, child: 276.0 },
        }
    }

    pub fn compact() -> Self {
        Self {
            column_gap: 44.0, row_gap: 18.0,
            root_radius: 180.0, fan_angle_step: 24.0, fan_max_angle: 160.0,
            radial_gap: 16.0, radial_step: 10.0, path_parent_gap: 32.0,
            pad_x: 20.0, pad_y: 16.0,
            min_height: 40.0, hit_min_height: 44.0, nearest: 26.0,
            min_width: RoleWidths { root: 84.0, path: 68.0, selection: 78.0, child: 62.0 },
            max_width: RoleWidths { root: 165.0, path: 145.0, selection: 165.0, child: 138.0 },
            column_max_width: RoleWidths { root: 235.0, path: 218.0, selection: 235.0, child: 212.0 },
        }
    }
}

#[derive(Clone, Debug)]
pub struct PlacedLabel {
    pub node: usize,
    pub parent: Option<usize>,
    pub role: LabelRole,
    pub center: Point<f32>,
    /// Zero points right; π points left. Used to frame compact screens.
    pub angle: f32,
    pub lines: Vec<String>,
    pub count: Option<usize>,
    pub text_width: f32,
    pub text_height: f32,
    pub bounds: Bounds<f32>,
    pub hit: Bounds<f32>,
    pub opacity: f32,
}

#[derive(Clone, Debug)]
pub struct GraphEdge {
    pub from: usize,
    pub to: usize,
    pub start: Point<f32>,
    pub end: Point<f32>,
    pub on_path: bool,
    pub opacity: f32,
}

#[derive(Clone)]
pub struct GraphLayout {
    pub kind: GraphLayoutKind,
    pub labels: Vec<PlacedLabel>,
    pub edges: Arc<Vec<GraphEdge>>,
    pub path: Vec<usize>,
    pub nearest: f32,
    label_index: Vec<Option<usize>>,
}

impl GraphLayout {
    pub fn label(&self, node: usize) -> Option<&PlacedLabel> {
        self.label_index.get(node).copied().flatten().map(|index| &self.labels[index])
    }

    pub fn retain_labels(&mut self, mut keep: impl FnMut(&PlacedLabel) -> bool) {
        self.labels.retain(|label| keep(label));
        self.label_index.fill(None);
        for (index, label) in self.labels.iter().enumerate() {
            self.label_index[label.node] = Some(index);
        }
    }

    pub fn hit_test(&self, world: Point<f32>, zoom: f32) -> Option<usize> {
        let area = |b: &Bounds<f32>| b.size.width * b.size.height;
        self.labels.iter().filter(|label| label.hit.contains(&world))
            .min_by(|a, b| area(&a.hit).total_cmp(&area(&b.hit)))
            .or_else(|| {
                let reach = self.nearest / zoom;
                self.labels.iter().map(|label| (label, distance(label.center, world)))
                    .filter(|(_, d)| *d < reach)
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(label, _)| label)
            })
            .map(|label| label.node)
    }

    /// A display pose of this layout, preserving the target's node identities
    /// while moving labels and their connectors to the supplied centres.
    pub fn with_pose(&self, centers: &[Point<f32>], opacity: &[f32]) -> Self {
        let mut posed = self.clone();
        for label in &mut posed.labels {
            if let Some(&center) = centers.get(label.node) {
                let delta = point(center.x - label.center.x, center.y - label.center.y);
                label.center = center;
                label.bounds = shifted(label.bounds, delta);
                label.hit = shifted(label.hit, delta);
            }
            label.opacity = opacity.get(label.node).copied().unwrap_or(1.0);
        }
        let labels = &posed.labels;
        let label_index = &posed.label_index;
        for edge in Arc::make_mut(&mut posed.edges) {
            let from = &labels[label_index[edge.from].expect("edge parent is placed")];
            let to = &labels[label_index[edge.to].expect("edge child is placed")];
            edge.opacity = to.opacity;
            if self.kind == GraphLayoutKind::AdaptiveRadial {
                let (dx, dy) = (to.center.x - from.center.x, to.center.y - from.center.y);
                let a = ray_box_fraction(from.bounds, dx, dy);
                let b = ray_box_fraction(to.bounds, dx, dy);
                if a + b >= 1.0 { edge.opacity = 0.0; }
                edge.start = point(from.center.x + dx * a, from.center.y + dy * a);
                edge.end = point(to.center.x - dx * b, to.center.y - dy * b);
            } else {
                let old_from = self.label(edge.from).expect("edge parent is placed");
                let old_to = self.label(edge.to).expect("edge child is placed");
                edge.start = point(edge.start.x + from.center.x - old_from.center.x, edge.start.y + from.center.y - old_from.center.y);
                edge.end = point(edge.end.x + to.center.x - old_to.center.x, edge.end.y + to.center.y - old_to.center.y);
            }
        }
        posed
    }
}

pub fn graph_layout(tree: &GraphTree, selection: usize, params: &LayoutParams, metrics: &dyn LabelMetrics) -> GraphLayout {
    build_layout(tree, selection, params, metrics, GraphLayoutKind::Columns)
}

pub fn radial_graph_layout(tree: &GraphTree, selection: usize, params: &LayoutParams, metrics: &dyn LabelMetrics) -> GraphLayout {
    build_layout(tree, selection, params, metrics, GraphLayoutKind::AdaptiveRadial)
}

fn build_layout(tree: &GraphTree, selection: usize, params: &LayoutParams, metrics: &dyn LabelMetrics, kind: GraphLayoutKind) -> GraphLayout {
    let path = tree.ancestry(selection);
    let mut builder = Builder { tree, params, metrics, kind, selection, path: &path, labels: Vec::new(), label_index: vec![None; tree.nodes.len()], edges: Vec::new() };
    let root = builder.measure(0, LabelRole::Root);
    builder.place(root, None, point(0.0, 0.0), 0.0);
    match kind {
        GraphLayoutKind::Columns => builder.columns(0, 0.0, 0.0),
        GraphLayoutKind::AdaptiveRadial => {
            builder.radial(0);
            builder.radial_edges();
        }
    }
    let Builder { labels, edges, label_index, .. } = builder;
    let labels = labels.into_iter().map(|label| PlacedLabel { hit: hit_box(label.bounds, params.hit_min_height), ..label }).collect();
    GraphLayout { kind, labels, edges: Arc::new(edges), path, nearest: params.nearest, label_index }
}

struct Measured {
    node: usize,
    role: LabelRole,
    lines: Vec<String>,
    count: Option<usize>,
    text_width: f32,
    text_height: f32,
    width: f32,
    height: f32,
    slot_width: f32,
    slot_height: f32,
}

struct RadialChild {
    measured: Measured,
    center: Point<f32>,
    angle: f32,
    bounds: Bounds<f32>,
}

struct Builder<'a> {
    tree: &'a GraphTree,
    params: &'a LayoutParams,
    metrics: &'a dyn LabelMetrics,
    kind: GraphLayoutKind,
    selection: usize,
    path: &'a [usize],
    labels: Vec<PlacedLabel>,
    label_index: Vec<Option<usize>>,
    edges: Vec<GraphEdge>,
}

impl Builder<'_> {
    fn role(&self, node: usize, parent: usize) -> LabelRole {
        if node == self.selection { LabelRole::Selection }
        else if self.path.contains(&node) { LabelRole::Path }
        else if parent == self.selection { LabelRole::Fan }
        else { LabelRole::Context }
    }

    fn measure(&self, node: usize, role: LabelRole) -> Measured {
        let p = self.params;
        let data = &self.tree.nodes[node];
        let width = |text: &str| self.metrics.text_width(text, role);
        let max_width = if self.kind == GraphLayoutKind::Columns {
            p.column_max_width.of(role)
        } else {
            p.max_width.of(role)
        };
        let max_text = max_width - p.pad_x;
        // Browse rows already carry the count for the entire subtree.
        let count = (data.books > 0).then_some(data.books);
        let count_width = count.map_or(0.0, |count| self.metrics.count_width(count, role));
        let mut lines = wrap_label(&data.name, max_text, &width, if self.kind == GraphLayoutKind::Columns { 3 } else { 2 });
        let last = lines.len() - 1;
        if count.is_some() { lines[last] = truncate_label(&lines[last], max_text - count_width, &width); }
        let text_width = lines.iter().enumerate().map(|(i, line)| width(line) + if i == last { count_width } else { 0.0 }).fold(0.0, f32::max);
        let text_height = lines.len() as f32 * self.metrics.line_height(role);
        Measured {
            node, role, count, text_width, text_height,
            width: max_width.min(p.min_width.of(role).max(text_width + p.pad_x).max(count_width + p.pad_x)),
            height: p.min_height.max(text_height + p.pad_y),
            slot_width: 0.0, slot_height: 0.0, lines,
        }
    }

    fn place(&mut self, measured: Measured, parent: Option<usize>, center: Point<f32>, angle: f32) {
        let bounds = Bounds::new(point(center.x - measured.width / 2.0, center.y - measured.height / 2.0), size(measured.width, measured.height));
        self.label_index[measured.node] = Some(self.labels.len());
        self.labels.push(PlacedLabel {
            node: measured.node, parent, role: measured.role, center, angle,
            lines: measured.lines, count: measured.count, text_width: measured.text_width,
            text_height: measured.text_height, bounds, hit: bounds, opacity: 1.0,
        });
    }

    fn placed(&self, node: usize) -> &PlacedLabel {
        &self.labels[self.label_index[node].expect("parent is placed before children")]
    }

    /// Use the nearest clear point on each spoke, then give every open
    /// non-root node with children more room along its incoming edge.
    fn radial(&mut self, node: usize) {
        let children = self.tree.nodes[node].children.clone();
        if children.is_empty() { return; }
        let parent = self.placed(node).clone();
        let root = node == 0;
        let path_fan = !root && self.path.contains(&node);
        let count = children.len();
        let span = if root { std::f32::consts::TAU } else {
            ((count - 1) as f32 * self.params.fan_angle_step)
                .min(self.params.fan_max_angle).to_radians()
        };
        let start = if root { -std::f32::consts::PI * 0.62 } else {
            parent.center.y.atan2(parent.center.x) - span / 2.0
        };
        let mut placed = Vec::with_capacity(count);
        for (index, &child) in children.iter().enumerate() {
            let role = self.role(child, node);
            let measured = self.measure(child, role);
            let angle = start + if root { span * index as f32 / count as f32 }
                else if count == 1 { 0.0 } else { span * index as f32 / (count - 1) as f32 };
            let candidate = |radius: f32| {
                let center = point(parent.center.x + radius * angle.cos(), parent.center.y + radius * angle.sin());
                (center, measured_bounds(&measured, center))
            };
            let clear = |radius: f32, placed: &[RadialChild]| {
                let (_, bounds) = candidate(radius);
                self.labels.iter().all(|other| path_fan && other.node != node || !overlaps(bounds, other.bounds, self.params.radial_gap))
                    && placed.iter().all(|other| !overlaps(bounds, other.bounds, self.params.radial_gap))
            };
            let mut radius = if root { self.params.root_radius } else { 1.0 };
            let mut last_blocked = radius;
            while !clear(radius, &placed) {
                last_blocked = radius;
                radius += self.params.radial_step;
            }
            if radius > last_blocked {
                let (mut lo, mut hi) = (last_blocked, radius);
                for _ in 0..10 {
                    let mid = (lo + hi) / 2.0;
                    if clear(mid, &placed) { hi = mid; } else { lo = mid; }
                }
                radius = hi;
            }
            let (center, bounds) = candidate(radius);
            placed.push(RadialChild { measured, center, angle, bounds });
        }

        if path_fan { self.lengthen_path_node(node, &mut placed); }
        for child in placed {
            self.place(child.measured, Some(node), child.center, child.angle);
        }
        for child in children {
            if self.path.contains(&child) { self.radial(child); }
        }
    }

    fn lengthen_path_node(&mut self, node: usize, children: &mut [RadialChild]) {
        let index = self.label_index[node].expect("path node is placed");
        let original = self.labels[index].clone();
        let parent = self.placed(original.parent.expect("non-root path node has parent")).center;
        let angle = (original.center.y - parent.y).atan2(original.center.x - parent.x);
        let (ux, uy) = (angle.cos(), angle.sin());
        let outside: Vec<Bounds<f32>> = self.labels.iter().filter(|label| label.node != node).map(|label| label.bounds).collect();
        let clear = |shift: f32| {
            let delta = point(shift * ux, shift * uy);
            let parent_box = shifted(original.bounds, delta);
            !outside.iter().any(|&other| overlaps(parent_box, other, self.params.radial_gap))
                && children.iter().all(|child| {
                    let box_at = shifted(child.bounds, delta);
                    !outside.iter().any(|&other| overlaps(box_at, other, self.params.radial_gap))
                })
        };
        let mut shift = self.params.path_parent_gap;
        let mut last_blocked = shift;
        while !clear(shift) {
            last_blocked = shift;
            shift += self.params.radial_step;
        }
        if shift > last_blocked {
            let (mut lo, mut hi) = (last_blocked, shift);
            for _ in 0..10 {
                let mid = (lo + hi) / 2.0;
                if clear(mid) { hi = mid; } else { lo = mid; }
            }
            shift = hi;
        }
        let delta = point(shift * ux, shift * uy);
        self.labels[index].center = point(original.center.x + delta.x, original.center.y + delta.y);
        self.labels[index].bounds = shifted(original.bounds, delta);
        for child in children {
            child.center = point(child.center.x + delta.x, child.center.y + delta.y);
            child.bounds = shifted(child.bounds, delta);
        }
    }

    fn radial_edges(&mut self) {
        for child in self.labels.iter().filter(|label| label.parent.is_some()) {
            let parent = self.placed(child.parent.expect("child has parent"));
            let (dx, dy) = (child.center.x - parent.center.x, child.center.y - parent.center.y);
            let from = ray_box_fraction(parent.bounds, dx, dy);
            let to = ray_box_fraction(child.bounds, dx, dy);
            let edge = GraphEdge {
                from: parent.node, to: child.node,
                start: point(parent.center.x + dx * from, parent.center.y + dy * from),
                end: point(child.center.x - dx * to, child.center.y - dy * to),
                on_path: self.path.contains(&parent.node) && self.path.contains(&child.node),
                opacity: 1.0,
            };
            self.edges.push(edge);
        }
    }

    /// `side` is zero at the root, then +1 or -1. `outer` is the far edge of
    /// the parent's column, including the widest role its siblings may take.
    fn columns(&mut self, node: usize, side: f32, outer: f32) {
        let children = &self.tree.nodes[node].children;
        if children.is_empty() { return; }
        let parent = self.placed(node);
        let (parent_center, parent_text_width) = (parent.center, parent.text_width);
        let mut kids: Vec<Measured> = children.iter().map(|&child| {
            let mut measured = self.measure(child, self.role(child, node));
            for role in [LabelRole::Selection, LabelRole::Path, LabelRole::Fan, LabelRole::Context] {
                let variant = self.measure(child, role);
                measured.slot_width = measured.slot_width.max(variant.width);
                measured.slot_height = measured.slot_height.max(variant.height);
            }
            measured
        }).collect();

        let split = if side == 0.0 {
            let total: f32 = kids.iter().map(|kid| kid.slot_height + self.params.row_gap).sum();
            let mut sum = 0.0;
            let mut best = (f32::INFINITY, 0);
            for (i, kid) in kids.iter().enumerate() {
                let diff = (total - 2.0 * sum).abs();
                if diff < best.0 { best = (diff, i); }
                sum += kid.slot_height + self.params.row_gap;
            }
            let diff = (total - 2.0 * sum).abs();
            if diff < best.0 { kids.len() } else { best.1 }
        } else { kids.len() };

        let groups = if side == 0.0 { [(0, split, 1.0), (split, kids.len(), -1.0)] }
                     else { [(0, kids.len(), side), (0, 0, side)] };
        for (start, end, direction) in groups {
            let group = &mut kids[start..end];
            if group.is_empty() { continue; }
            let height: f32 = group.iter().map(|kid| kid.slot_height).sum::<f32>() + self.params.row_gap * (group.len() - 1) as f32;
            let parent_outer = if side == 0.0 { parent_center.x + direction * parent_text_width / 2.0 } else { outer };
            let edge = parent_outer + direction * self.params.column_gap;
            let column_outer = edge + direction * group.iter().map(|kid| kid.slot_width).fold(0.0, f32::max);
            let mut y = parent_center.y - height / 2.0;
            for kid in group.iter() {
                let center = point(edge + direction * kid.slot_width / 2.0, y + kid.slot_height / 2.0);
                let from_x = parent_center.x + direction * (parent_text_width / 2.0 + 8.0);
                let to_x = center.x - direction * (kid.text_width / 2.0 + 8.0);
                self.edges.push(GraphEdge {
                    from: node, to: kid.node, start: point(from_x, parent_center.y), end: point(to_x, center.y),
                    on_path: self.path.contains(&node) && self.path.contains(&kid.node),
                    opacity: 1.0,
                });
                self.place(self.measure(kid.node, kid.role), Some(node), center, if direction > 0.0 { 0.0 } else { std::f32::consts::PI });
                y += kid.slot_height + self.params.row_gap;
            }
            for kid in group.iter() {
                if self.path.contains(&kid.node) { self.columns(kid.node, direction, column_outer); }
            }
        }
    }
}

fn distance(a: Point<f32>, b: Point<f32>) -> f32 { (a.x - b.x).hypot(a.y - b.y) }

fn measured_bounds(label: &Measured, center: Point<f32>) -> Bounds<f32> {
    Bounds::new(point(center.x - label.width / 2.0, center.y - label.height / 2.0), size(label.width, label.height))
}

fn shifted(bounds: Bounds<f32>, delta: Point<f32>) -> Bounds<f32> {
    Bounds::new(point(bounds.origin.x + delta.x, bounds.origin.y + delta.y), bounds.size)
}

fn overlaps(a: Bounds<f32>, b: Bounds<f32>, gap: f32) -> bool {
    a.origin.x < b.origin.x + b.size.width + gap && b.origin.x < a.origin.x + a.size.width + gap
        && a.origin.y < b.origin.y + b.size.height + gap && b.origin.y < a.origin.y + a.size.height + gap
}

fn ray_box_fraction(bounds: Bounds<f32>, dx: f32, dy: f32) -> f32 {
    let fx = if dx.abs() > f32::EPSILON { bounds.size.width / (2.0 * dx.abs()) } else { f32::INFINITY };
    let fy = if dy.abs() > f32::EPSILON { bounds.size.height / (2.0 * dy.abs()) } else { f32::INFINITY };
    fx.min(fy).min(0.5)
}

fn hit_box(bounds: Bounds<f32>, min_height: f32) -> Bounds<f32> {
    let height = bounds.size.height.max(min_height);
    let center_y = bounds.origin.y + bounds.size.height / 2.0;
    Bounds::new(point(bounds.origin.x - 4.0, center_y - height / 2.0), size(bounds.size.width + 8.0, height))
}

fn wrap_label(text: &str, max_width: f32, width: &dyn Fn(&str) -> f32, max_lines: usize) -> Vec<String> {
    if width(text) <= max_width { return vec![text.to_string()]; }
    let words: Vec<&str> = text.split(' ').collect();
    if words.len() == 1 { return vec![truncate_label(text, max_width, width)]; }
    if max_lines == 3 {
        let mut lines = Vec::new();
        let mut current = String::new();
        for word in words {
            let candidate = if current.is_empty() { word.to_string() } else { format!("{current} {word}") };
            if !current.is_empty() && width(&candidate) > max_width && lines.len() < 2 {
                lines.push(current);
                current = word.to_string();
            } else {
                current = candidate;
            }
        }
        lines.push(current);
        return lines.into_iter().map(|line| truncate_label(&line, max_width, width)).collect();
    }
    let longest = |lines: &[String; 2]| width(&lines[0]).max(width(&lines[1]));
    let best = (1..words.len()).map(|i| [words[..i].join(" "), words[i..].join(" ")])
        .min_by(|a, b| longest(a).total_cmp(&longest(b))).expect("at least two words");
    best.iter().map(|line| truncate_label(line, max_width, width)).collect()
}

fn truncate_label(text: &str, max_width: f32, width: &dyn Fn(&str) -> f32) -> String {
    if width(text) <= max_width { return text.to_string(); }
    let mut chars: Vec<char> = text.trim_end_matches('…').chars().collect();
    while chars.len() > 1 && width(&format!("{}…", chars.iter().collect::<String>())) > max_width { chars.pop(); }
    format!("{}…", chars.iter().collect::<String>().trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::tree::GraphNode;

    struct FixedMetrics;

    impl LabelMetrics for FixedMetrics {
        fn text_width(&self, text: &str, _: LabelRole) -> f32 { text.chars().count() as f32 * 8.0 }
        fn count_width(&self, _: usize, _: LabelRole) -> f32 { 0.0 }
        fn line_height(&self, _: LabelRole) -> f32 { 16.0 }
    }

    #[test]
    fn columns_show_more_of_long_names_than_radial() {
        let name = "Studies in the History of Philosophy and the Natural Sciences";
        let tree = GraphTree::new(vec![
            GraphNode { name: "Library".into(), parent: None, children: Vec::new(), books: 0 },
            GraphNode { name: name.into(), parent: Some(0), children: Vec::new(), books: 0 },
        ]);
        let params = LayoutParams::desktop();
        let columns = graph_layout(&tree, 0, &params, &FixedMetrics);
        let radial = radial_graph_layout(&tree, 0, &params, &FixedMetrics);
        let column_label = columns.label(1).unwrap();
        let radial_label = radial.label(1).unwrap();
        assert_eq!(column_label.lines.len(), 3);
        assert_eq!(column_label.lines.join(" "), name);
        assert_eq!(radial_label.lines.len(), 2);
        assert!(radial_label.lines.iter().any(|line| line.ends_with('…')));
        assert!(column_label.bounds.size.width > radial_label.bounds.size.width);
    }
}

//! The graph view of a browse page: the root, the path to the location being
//! browsed, and the children of each, on an infinite canvas.
//!
//! The location being browsed is the graph's selection. Tapping or navigating
//! to a node browses it; the graph then moves that node to the camera centre.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{AnyElement, App, AppContext as _, Bounds, Entity, IntoElement, Pixels, Point, Window, point, px};
use library_model::BrowseRow;
use ui_components::{self as components, CanvasCamera, GraphLayout, GraphLayoutKind, GraphMetrics, GraphNode, GraphTree, GraphType, InfiniteCanvas, LayoutParams};

use super::listing::BrowseCrumb;

const MIN_ZOOM: f32 = 0.3;
const MAX_ZOOM: f32 = 2.5;
const PIN_DURATION: Duration = Duration::from_millis(60);
const MOVE_DURATION: Duration = Duration::from_millis(300);

/// What a tap on the graph asks the page to do.
pub(super) enum GraphTap {
    /// Browse this trail's location.
    Select(Vec<BrowseCrumb>),
    /// The selection was tapped again: open it in the cards view.
    Open,
}

/// A drawn layout and, for each of its nodes, the trail that browses it.
struct Drawn {
    layout: GraphLayout,
    trails: Arc<Vec<Vec<BrowseCrumb>>>,
    keys: Arc<Vec<String>>,
}

struct CachedGraph {
    root: String,
    root_label: String,
    root_book_count: usize,
    path: Vec<BrowseCrumb>,
    compact: bool,
    rem_size: Pixels,
    kind: GraphLayoutKind,
    layout: Arc<GraphLayout>,
    trails: Arc<Vec<Vec<BrowseCrumb>>>,
    keys: Arc<Vec<String>>,
}

impl CachedGraph {
    fn matches(&self, root: &str, root_label: &str, root_book_count: usize, path: &[BrowseCrumb], compact: bool, rem_size: Pixels, kind: GraphLayoutKind) -> bool {
        self.root == root && self.root_label == root_label && self.root_book_count == root_book_count
            && self.path == path && self.compact == compact && self.rem_size == rem_size && self.kind == kind
    }
}

struct GraphMotion {
    started: Instant,
    from: Vec<Point<f32>>,
    appearing: Vec<bool>,
    from_camera: CanvasCamera,
    last_camera: CanvasCamera,
    target_camera: CanvasCamera,
    selected_key: String,
    anchor: Point<Pixels>,
    departing: GraphLayout,
    ghost_opacity: f32,
}

pub(super) struct BrowseGraph {
    canvas: Entity<InfiniteCanvas>,
    layout_kind: GraphLayoutKind,
    /// The children of every location loaded, so the path and its sibling fans
    /// draw without asking again.
    children: HashMap<String, Vec<BrowseRow>>,
    /// Locations whose children are being fetched.
    fetching: HashSet<String>,
    target: Option<CachedGraph>,
    drawn: Option<Drawn>,
    hovered: Option<usize>,
    /// The next selected location to centre after its children have loaded.
    center_for: Option<String>,
    /// The selection the camera was last placed for.
    placed_for: Option<String>,
    motion: Option<GraphMotion>,
}

impl BrowseGraph {
    pub(super) fn new(layout_kind: GraphLayoutKind, cx: &mut App) -> Self {
        Self { canvas: cx.new(|_| InfiniteCanvas::new(MIN_ZOOM, MAX_ZOOM)), layout_kind, children: HashMap::new(), fetching: HashSet::new(), target: None, drawn: None, hovered: None, center_for: None, placed_for: None, motion: None }
    }

    pub(super) fn layout_kind(&self) -> GraphLayoutKind { self.layout_kind }

    pub(super) fn set_layout_kind(&mut self, kind: GraphLayoutKind) {
        if self.layout_kind == kind { return; }
        self.layout_kind = kind;
        self.target = None;
        self.placed_for = None;
        self.hovered = None;
        self.center_for = self.drawn.as_ref().and_then(|drawn| drawn.layout.path.last().map(|&id| drawn.trails[id].last().map_or(String::new(), |crumb| crumb.location.clone())));
    }

    pub(super) fn remember(&mut self, location: &str, children: Vec<BrowseRow>) {
        self.fetching.remove(location);
        self.children.insert(location.to_owned(), children);
        self.target = None;
    }

    /// Locations on the way to the selection whose children are neither known
    /// nor being fetched. Each is marked as being fetched.
    pub(super) fn take_missing(&mut self, root: &str, path: &[BrowseCrumb]) -> Vec<String> {
        let locations = std::iter::once(root.to_owned()).chain(path.iter().map(|crumb| crumb.location.clone()));
        let missing: Vec<String> = locations.filter(|location| !self.children.contains_key(location) && !self.fetching.contains(location)).collect();
        self.fetching.extend(missing.iter().cloned());
        missing
    }

    /// A fetch that failed can be tried again the next time the graph is drawn.
    pub(super) fn forget_fetch(&mut self, location: &str) {
        self.fetching.remove(location);
    }

    pub(super) fn tap(&mut self, world: Point<Pixels>, cx: &App) -> Option<GraphTap> {
        let drawn = self.drawn.as_ref()?;
        let camera = self.canvas.read(cx).camera;
        let node = drawn.layout.hit_test(point(f32::from(world.x), f32::from(world.y)), camera.zoom)?;
        self.activate(node)
    }

    /// Updates the label under the pointer. Repaint only when it changes.
    pub(super) fn hover(&mut self, world: Option<Point<Pixels>>, cx: &App) -> bool {
        let hovered = world.and_then(|world| {
            let drawn = self.drawn.as_ref()?;
            let zoom = self.canvas.read(cx).camera.zoom;
            drawn.layout.hit_test(point(f32::from(world.x), f32::from(world.y)), zoom)
        });
        if self.hovered == hovered { return false; }
        self.hovered = hovered;
        true
    }

    /// Arrows walk the hierarchy and its ordered column. Page Up/Down (also
    /// sent by Android's volume buttons) move to the parent/first child.
    /// Enter opens the selected location in cards.
    pub(super) fn key(&mut self, key: &str) -> Option<GraphTap> {
        let drawn = self.drawn.as_ref()?;
        let selection = *drawn.layout.path.last()?;
        let selected_label = drawn.layout.label(selection)?;
        let parent = selected_label.parent;
        if key == "pageup" {
            return self.activate(parent?);
        }
        if key == "pagedown" {
            let child = drawn.layout.labels.iter()
                .find(|label| label.parent == Some(selection))?
                .node;
            return self.activate(child);
        }
        if drawn.layout.kind == GraphLayoutKind::AdaptiveRadial {
            let children = || drawn.layout.labels.iter().find(|label| label.parent == Some(selection)).map(|label| label.node);
            let node = match key {
                "left" => parent?,
                "right" => children()?,
                "up" | "down" => {
                    let parent = parent?;
                    let siblings: Vec<usize> = drawn.layout.labels.iter().filter(|label| label.parent == Some(parent)).map(|label| label.node).collect();
                    let index = siblings.iter().position(|&node| node == selection)?;
                    siblings[if key == "down" { (index + 1) % siblings.len() } else { (index + siblings.len() - 1) % siblings.len() }]
                }
                "enter" => return Some(GraphTap::Open),
                _ => return None,
            };
            return self.activate(node);
        }
        let side = if selected_label.angle.cos() < 0.0 { -1.0 } else { 1.0 };
        let first_child = |direction: f32| {
            drawn.layout.labels.iter()
                .find(|label| label.parent == Some(selection) && label.angle.cos() * direction > 0.0)
                .map(|label| label.node)
        };
        let node = match key {
            "left" if selection == 0 => first_child(-1.0)?,
            "right" if selection == 0 => first_child(1.0)?,
            "left" if side < 0.0 => first_child(side)?,
            "right" if side < 0.0 => parent?,
            "left" => parent?,
            "right" => first_child(side)?,
            "up" | "down" => {
                let siblings: Vec<usize> = drawn.layout.labels.iter()
                    .filter(|label| label.parent == parent && parent.is_some() && label.angle.cos() * side > 0.0)
                    .map(|label| label.node).collect();
                let index = siblings.iter().position(|&node| node == selection)?;
                let next = if key == "down" { (index + 1) % siblings.len() } else { (index + siblings.len() - 1) % siblings.len() };
                siblings[next]
            }
            "enter" => return Some(GraphTap::Open),
            _ => return None,
        };
        if node == selection { return None; }
        self.activate(node)
    }

    fn activate(&mut self, node: usize) -> Option<GraphTap> {
        let drawn = self.drawn.as_ref()?;
        if Some(&node) == drawn.layout.path.last() { return Some(GraphTap::Open); }
        drawn.layout.label(node)?;
        let trail = drawn.trails[node].clone();
        let location = trail.last().map(|crumb| crumb.location.clone()).unwrap_or_default();
        self.hovered = None;
        self.center_for = Some(location);
        self.motion = None;
        Some(GraphTap::Select(trail))
    }

    /// The graph for `path` below `root`. Until every column along the path is
    /// known, the last graph drawn stays up.
    pub(super) fn render(
        &mut self, root: &str, root_label: &str, root_book_count: usize, path: &[BrowseCrumb],
        on_tap: impl Fn(Point<Pixels>, &mut Window, &mut App) + 'static,
        on_hover: impl Fn(Option<Point<Pixels>>, &mut Window, &mut App) + 'static,
        window: &mut Window, cx: &mut App,
    ) -> AnyElement {
        let compact = components::WindowWidthClass::for_window(window).is_compact();
        let (params, sizes) = if compact { (LayoutParams::compact(), GraphType::compact()) } else { (LayoutParams::desktop(), GraphType::desktop()) };
        let target_changed = !self.target.as_ref().is_some_and(|target| target.matches(root, root_label, root_book_count, path, compact, window.rem_size(), self.layout_kind));
        if target_changed {
            self.target = graph_tree(root, root_label, root_book_count, path, &self.children).map(|(tree, trails, selection)| {
                let keys: Vec<String> = trails.iter().map(|trail| trail_key(root, trail)).collect();
                let metrics = GraphMetrics { window, sizes };
                let layout = match self.layout_kind {
                    GraphLayoutKind::Columns => components::graph_layout(&tree, selection, &params, &metrics),
                    GraphLayoutKind::AdaptiveRadial => components::radial_graph_layout(&tree, selection, &params, &metrics),
                };
                CachedGraph {
                    root: root.to_owned(), root_label: root_label.to_owned(), root_book_count, path: path.to_vec(),
                    compact, rem_size: window.rem_size(), kind: self.layout_kind,
                    layout: Arc::new(layout), trails: Arc::new(trails), keys: Arc::new(keys),
                }
            });
        }
        if let Some(target) = &self.target {
            let layout = Arc::clone(&target.layout);
            let trails = Arc::clone(&target.trails);
            let keys = Arc::clone(&target.keys);
            let location = path.last().map_or(root, |crumb| crumb.location.as_str()).to_owned();
            let center_new = self.center_for.as_deref().is_some_and(|target| target == location.as_str() || (path.is_empty() && target.is_empty()));
            if self.placed_for.as_ref() != Some(&location) || center_new {
                self.place_camera(&layout, &keys, &location, compact, center_new, window, cx);
            }
            if target_changed || self.motion.is_some() || self.drawn.is_none() {
                let layout = self.animated_layout(&layout, &keys, window, cx);
                self.drawn = Some(Drawn { layout, trails, keys });
            }
        }
        let Some(drawn) = &self.drawn else {
            return components::infinite_canvas(&self.canvas).into_any_element();
        };
        let ghost = self.motion.as_ref().and_then(|motion| {
            if motion.departing.labels.is_empty() || motion.ghost_opacity <= 0.0 { return None; }
            Some((&motion.departing, motion.ghost_opacity))
        });
        components::graph_scene(&self.canvas, &drawn.layout, ghost, self.hovered, sizes, window, cx).on_tap(on_tap).on_hover(on_hover).into_any_element()
    }

    /// Centres a newly selected node, or frames the initial selection.
    /// Camera placement waits for the canvas's first layout.
    fn place_camera(&mut self, layout: &GraphLayout, keys: &[String], location: &str, compact: bool, center_new: bool, window: &Window, cx: &mut App) {
        let selection = *layout.path.last().expect("the path always holds the root");
        let Some(bounds) = self.canvas.read(cx).bounds else {
            window.request_animation_frame();
            return;
        };
        let camera = if center_new {
            let anchor = point(bounds.size.width / 2.0, bounds.size.height / 2.0);
            let zoom = self.canvas.read(cx).camera.zoom;
            components::anchored_camera(layout, selection, anchor, zoom).expect("the selected node is placed")
        } else {
            components::framing_camera(layout, Bounds::new(point(px(0.0), px(0.0)), bounds.size), compact, 1.0)
        };
        let from_camera = self.canvas.read(cx).camera;
        let selected_key = &keys[selection];
        let previous = self.drawn.as_ref().and_then(|drawn| {
            drawn.layout.labels.iter().find(|label| drawn.keys[label.node] == *selected_key)
                .map(|label| (drawn, label.center))
        });
        if center_new && !cx.reduce_motion() && !cfg!(feature = "kobo") {
            if let Some((drawn, old_center)) = previous {
                let old_centers: HashMap<&str, Point<f32>> = drawn.layout.labels.iter()
                    .map(|label| (drawn.keys[label.node].as_str(), label.center)).collect();
                let mut from = vec![point(0.0, 0.0); keys.len()];
                let mut appearing = vec![false; keys.len()];
                for label in &layout.labels {
                    let key = keys[label.node].as_str();
                    from[label.node] = old_centers.get(key).copied().or_else(|| {
                        label.parent.and_then(|parent| old_centers.get(keys[parent].as_str()).copied())
                    }).unwrap_or(label.center);
                    appearing[label.node] = !old_centers.contains_key(key);
                }
                let anchor = from_camera.to_screen(point(px(old_center.x), px(old_center.y)));
                let visible: HashSet<&str> = keys.iter().map(String::as_str).collect();
                let mut departing = drawn.layout.clone();
                departing.retain_labels(|label| !visible.contains(drawn.keys[label.node].as_str()));
                Arc::make_mut(&mut departing.edges).retain(|edge| !visible.contains(drawn.keys[edge.to].as_str()));
                self.motion = Some(GraphMotion {
                    started: Instant::now(), from, appearing, from_camera, last_camera: from_camera,
                    target_camera: camera, selected_key: selected_key.clone(), anchor,
                    departing, ghost_opacity: 1.0,
                });
            } else {
                self.motion = None;
                self.canvas.update(cx, |canvas, _| canvas.camera = camera);
            }
        } else {
            self.motion = None;
            self.canvas.update(cx, |canvas, _| canvas.camera = camera);
        }
        self.center_for = None;
        self.placed_for = Some(location.to_owned());
    }

    fn animated_layout(&mut self, layout: &GraphLayout, keys: &[String], window: &Window, cx: &mut App) -> GraphLayout {
        let Some(motion) = &mut self.motion else { return layout.clone(); };
        let selection = *layout.path.last().expect("the path always holds the root");
        if keys[selection] != motion.selected_key || self.canvas.read(cx).camera != motion.last_camera {
            // A wheel, pinch, or drag takes control of the camera immediately.
            self.motion = None;
            return layout.clone();
        }
        let elapsed = motion.started.elapsed();
        motion.ghost_opacity = 1.0 - (elapsed.as_secs_f32() / 0.12).clamp(0.0, 1.0);
        let progress = (elapsed.saturating_sub(PIN_DURATION).as_secs_f32() / MOVE_DURATION.as_secs_f32()).clamp(0.0, 1.0);
        let mut centers = vec![point(0.0, 0.0); keys.len()];
        let mut opacity = vec![1.0; keys.len()];
        for label in &layout.labels {
            let old = motion.from[label.node];
            centers[label.node] = point(old.x + (label.center.x - old.x) * progress, old.y + (label.center.y - old.y) * progress);
            if motion.appearing[label.node] { opacity[label.node] = progress; }
        }
        let posed = layout.with_pose(&centers, &opacity);
        let selected = posed.label(selection).expect("selected node is placed").center;
        let target = layout.label(selection).expect("selected node is placed").center;
        let end = motion.target_camera.to_screen(point(px(target.x), px(target.y)));
        let zoom = motion.from_camera.zoom + (motion.target_camera.zoom - motion.from_camera.zoom) * progress;
        let screen = point(
            motion.anchor.x + (end.x - motion.anchor.x) * progress,
            motion.anchor.y + (end.y - motion.anchor.y) * progress,
        );
        let camera = CanvasCamera { pan: point(screen.x - px(selected.x * zoom), screen.y - px(selected.y * zoom)), zoom };
        self.canvas.update(cx, |canvas, _| canvas.camera = camera);
        motion.last_camera = camera;
        if progress < 1.0 {
            window.request_animation_frame();
            posed
        } else {
            self.motion = None;
            layout.clone()
        }
    }
}

fn trail_key(root: &str, trail: &[BrowseCrumb]) -> String {
    let mut key = root.to_owned();
    for crumb in trail {
        key.push('\0');
        key.push_str(&crumb.location);
    }
    key
}

/// The tree the graph draws: the root, the children of the root and of every
/// location on `path`, and the index of the path's last location. `None` while
/// any of those locations' children are unknown.
fn graph_tree(root: &str, root_label: &str, root_book_count: usize, path: &[BrowseCrumb], children: &HashMap<String, Vec<BrowseRow>>) -> Option<(GraphTree, Vec<Vec<BrowseCrumb>>, usize)> {
    let node = |name: &str, parent, books| GraphNode { name: name.to_owned(), parent, children: Vec::new(), books };
    let mut nodes = vec![node(root_label, None, root_book_count)];
    let mut trails: Vec<Vec<BrowseCrumb>> = vec![Vec::new()];
    let mut parent = 0;
    let mut location = root;
    for depth in 0..=path.len() {
        let next = path.get(depth);
        let mut next_index = None;
        for row in children.get(location)? {
            if next.is_some_and(|crumb| crumb.location == row.id) {
                next_index = Some(nodes.len());
            }
            nodes.push(node(&row.name, Some(parent), row.book_count));
            trails.push([trails[parent].clone(), vec![BrowseCrumb { location: row.id.clone(), label: row.name.clone() }]].concat());
        }
        let Some(crumb) = next else { break };
        // A location reached some other way than through its parent's children
        // — a search hit's trail — still belongs on the path.
        parent = next_index.unwrap_or_else(|| {
            nodes.push(node(&crumb.label, Some(parent), 0));
            trails.push([trails[parent].clone(), vec![crumb.clone()]].concat());
            nodes.len() - 1
        });
        location = &crumb.location;
    }
    Some((GraphTree::new(nodes), trails, parent))
}

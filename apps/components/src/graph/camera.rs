//! Where to put the canvas camera for a graph layout.

use gpui::{Bounds, Pixels, Point, point, px};

use super::layout::GraphLayout;
use crate::CanvasCamera;

/// Screen margin kept between the framed children and the visible area.
const MARGIN: f32 = 14.0;

/// Frames the selection and its children in `area`, the part of the canvas that is
/// not covered by overlays, in canvas-relative pixels. `user_zoom` scales the fit.
///
/// On compact screens the selection sits back from the centre, opposite the
/// direction its column opens, and its children stay on screen where they fit.
pub fn framing_camera(layout: &GraphLayout, area: Bounds<Pixels>, compact: bool, user_zoom: f32) -> CanvasCamera {
    let selection = *layout.path.last().expect("the path always holds the root");
    let s = layout.label(selection).expect("the selection is always placed");
    let fan = layout.labels.iter().filter(|label| label.node == selection || label.parent == Some(selection));
    let (x0, y0, x1, y1) = fan.fold((f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY), |(x0, y0, x1, y1), label| {
        let h = label.hit;
        (x0.min(h.origin.x), y0.min(h.origin.y), x1.max(h.origin.x + h.size.width), y1.max(h.origin.y + h.size.height))
    });

    let (left, top) = (f32::from(area.origin.x), f32::from(area.origin.y));
    let (width, height) = (f32::from(area.size.width), f32::from(area.size.height));
    let (mut ax, mut ay) = (left + width / 2.0, top + height / 2.0);
    if compact && selection != 0 {
        let k = width.min(height) * 0.26;
        ax -= s.angle.cos() * k;
        ay -= s.angle.sin() * k;
    }

    let (sx, sy) = (s.center.x, s.center.y);
    let fits = [
        (x1 > sx).then(|| (left + width - MARGIN - ax) / (x1 - sx)),
        (x0 < sx).then(|| (ax - left - MARGIN) / (sx - x0)),
        (y1 > sy).then(|| (top + height - MARGIN - ay) / (y1 - sy)),
        (y0 < sy).then(|| (ay - top - MARGIN) / (sy - y0)),
    ];
    let (min_fit, max_fit) = if compact { (0.85, 1.1) } else { (0.45, 1.0) };
    let fit = fits.into_iter().flatten().fold(max_fit, f32::min).max(min_fit);
    let zoom = fit * user_zoom;

    let (mut tx, mut ty) = (ax - sx * zoom, ay - sy * zoom);
    if compact {
        tx = keep_visible(x0, x1, tx, left, width, zoom);
        ty = keep_visible(y0, y1, ty, top, height, zoom);
    }
    CanvasCamera { pan: point(px(tx), px(ty)), zoom }
}

/// Keeps `node` at the canvas-relative `anchor` at the given zoom, so a tapped node
/// stays under the finger after the layout changes around it.
pub fn anchored_camera(layout: &GraphLayout, node: usize, anchor: Point<Pixels>, zoom: f32) -> Option<CanvasCamera> {
    let label = layout.label(node)?;
    Some(CanvasCamera { pan: point(anchor.x - px(label.center.x * zoom), anchor.y - px(label.center.y * zoom)), zoom })
}

/// Shifts one axis of the pan so the world span `lo..hi` stays inside the screen
/// span `start..start + size`, or centres it when it cannot fit.
fn keep_visible(lo: f32, hi: f32, offset: f32, start: f32, size: f32, zoom: f32) -> f32 {
    let min_offset = start + MARGIN - lo * zoom;
    let max_offset = start + size - MARGIN - hi * zoom;
    if min_offset <= max_offset { offset.clamp(min_offset, max_offset) } else { start + size / 2.0 - (lo + hi) * zoom / 2.0 }
}

#[cfg(test)]
mod tests {
    use gpui::{size, Bounds};

    use super::*;
    use crate::graph::layout::{LabelMetrics, LabelRole, LayoutParams, graph_layout};
    use crate::graph::tree::{GraphNode, GraphTree};

    struct FixedMetrics;

    impl LabelMetrics for FixedMetrics {
        fn text_width(&self, text: &str, _: LabelRole) -> f32 {
            text.chars().count() as f32 * 8.0
        }

        fn count_width(&self, _: usize, _: LabelRole) -> f32 {
            10.0
        }

        fn line_height(&self, _: LabelRole) -> f32 {
            16.0
        }
    }

    fn tree() -> GraphTree {
        let node = |name: &str, parent| GraphNode { name: name.to_string(), parent, children: Vec::new(), books: 2 };
        let mut nodes = vec![node("Library", None)];
        nodes.extend((0..8).map(|i| node(&format!("Branch {i}"), Some(0))));
        nodes.extend((0..5).map(|i| node(&format!("Leaf {i}"), Some(1))));
        GraphTree::new(nodes)
    }

    #[test]
    fn framing_puts_the_selection_inside_the_area() {
        let tree = tree();
        let selection = tree.nodes[0].children[0];
        let layout = graph_layout(&tree, selection, &LayoutParams::compact(), &FixedMetrics);
        let area = Bounds::new(point(px(0.0), px(60.0)), size(px(390.0), px(500.0)));
        let camera = framing_camera(&layout, area, true, 1.0);
        let center = layout.label(selection).unwrap().center;
        let on_screen = camera.to_screen(point(px(center.x), px(center.y)));
        assert!(area.contains(&on_screen), "{on_screen:?} is outside {area:?}");
    }

    #[test]
    fn anchoring_keeps_the_node_under_the_anchor() {
        let tree = tree();
        let layout = graph_layout(&tree, 0, &LayoutParams::desktop(), &FixedMetrics);
        let node = tree.nodes[0].children[2];
        let anchor = point(px(300.0), px(200.0));
        let camera = anchored_camera(&layout, node, anchor, 1.5).unwrap();
        let center = layout.label(node).unwrap().center;
        assert_eq!(camera.to_screen(point(px(center.x), px(center.y))), anchor);
    }
}

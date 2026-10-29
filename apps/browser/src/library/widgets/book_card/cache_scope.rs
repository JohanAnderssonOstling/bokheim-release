//! Give a cached presentation its invalidation identity without a layout box.

use gpui::{AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels, Window};

pub(super) struct CacheScope {
    id: ElementId,
    content: AnyElement,
}

impl CacheScope {
    pub(super) fn new(id: ElementId, content: AnyElement) -> Self {
        Self { id, content }
    }
}

impl IntoElement for CacheScope {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for CacheScope {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        // Share the cached view's layout node; this scope only changes the
        // element-state path when selection or typography changes.
        (self.content.request_layout(window, cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) {
        self.content.prepaint(window, cx);
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut (), window: &mut Window, cx: &mut App) {
        self.content.paint(window, cx);
    }
}

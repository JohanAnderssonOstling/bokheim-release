mod paint;
mod text;
mod view;

pub use text::TextShapeCacheStats;
pub use view::{DocumentTap, HtmlLayoutGeometry, HtmlNoteElement, HtmlView, PaintCacheStats, PreparedNote};

#[cfg(test)]
mod boundary_tests {
    #[test]
    fn gpui_adapter_has_no_direct_html_layout_or_style_dependencies() {
        let manifest = include_str!("../Cargo.toml");
        let dependencies = manifest.lines().filter_map(|line| line.split_once('=').map(|(name, _)| name.trim()));
        let forbidden = ["html-layout", "html-style", "html-style-model", "html-pipeline", "html-dom"];
        let violations = dependencies.filter(|name| forbidden.contains(name)).collect::<Vec<_>>();
        assert!(violations.is_empty(), "GPUI must consume the neutral text_backend contract, not HTML implementation crates: {violations:?}");
    }

    #[test]
    fn gpui_text_backend_contains_no_html_semantic_vocabulary() {
        let source = include_str!("text.rs");
        for forbidden in ["use html_view_core::{", "html_layout", "html_style", "html_pipeline", "html_dom", "StyleStringId", "letter_spacing", "word_spacing", "justification"] {
            assert!(!source.contains(forbidden), "GPUI text backend must not reference HTML-layer concept {forbidden:?}");
        }
    }

    #[test]
    fn gpui_prepaint_can_only_compose_a_pipeline_free_viewport() {
        let source = include_str!("view.rs");
        let element = source.find("impl Element for HtmlDocumentElement").expect("document element implementation");
        let prepaint = source[element..].find("fn prepaint(").map(|offset| element + offset).expect("document prepaint");
        let paint = source[prepaint..].find("fn paint(").map(|offset| prepaint + offset).expect("document paint");
        let prepaint_body = &source[prepaint..paint];
        assert!(prepaint_body.contains("prepared_paint_for_bounds"), "prepaint must compose from final layout bounds");
        assert!(!prepaint_body.contains("cx.defer"), "viewport composition must finish before the resized frame is painted");
        assert!(!prepaint_body.contains("prepare_frame"), "prepaint must not run renderer preparation");
        assert!(!prepaint_body.contains("update_resources"), "prepaint must not poll resources or enter the pipeline");

        assert_eq!(source.matches("renderer.prepare_frame").count(), 0, "the GPUI adapter must never call the pipeline-capable combined preparation API");
        assert_eq!(source.matches("renderer.prepare_viewport").count(), 1, "pure viewport preparation must have one auditable call site");
        let bounds_composer = source.find("fn prepared_paint_for_bounds").expect("post-layout viewport composer");
        let display_list_composer = source.find("fn compose_viewport_display_list").expect("pipeline-free display-list composer");
        let viewport_prepare = source.find("renderer.prepare_viewport").expect("viewport preparation call");
        assert!(bounds_composer < display_list_composer && display_list_composer < viewport_prepare, "viewport preparation must remain in the same-frame pipeline-free composer");
        let composer_end = source[display_list_composer..].find("fn handle_host_message").map(|offset| display_list_composer + offset).expect("host update boundary");
        let composer_body = &source[display_list_composer..composer_end];
        assert!(!composer_body.contains("update_resources"), "display-list composition must remain resource- and pipeline-free");
    }
}

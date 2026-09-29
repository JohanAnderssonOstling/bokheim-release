# Optimization Candidates

Prioritized list of remaining "do less" optimizations for the HTML renderer:

1. Tables
   - [`apps/reader/crates/html/src/layout/table_layout.rs`](./src/layout/table_layout.rs#L538) still does the measure-then-layout-again pattern for spanning tables.
   - This is duplicated work, not just bookkeeping.

2. Merge the two post-shape glyph passes
   - [`apps/reader/crates/html/src/renderer/load.rs`](./src/renderer/load.rs#L314) walks `inline_runs()` for `build_link_glyph_hrefs`.
   - [`apps/reader/crates/html/src/renderer/load.rs`](./src/renderer/load.rs#L350) walks `inline_runs()` again for `build_anchor_glyphs`.
   - Both passes climb parent chains over the same text runs and can likely be combined.

3. Make anchor position collection single-pass
   - [`apps/reader/crates/html/src/renderer/load.rs`](./src/renderer/load.rs#L267) scans all lines again for every anchor glyph.
   - Recording the first line position during layout would remove that extra search.

4. Trim the decoration walk
   - [`apps/reader/crates/html/src/layout/decorations.rs`](./src/layout/decorations.rs#L102) rebuilds an ancestor map and scans every glyph on every line.
   - More of this data could potentially be emitted while lines are already being produced.

5. Reduce repeated child classification in block flow
   - [`apps/reader/crates/html/src/layout/block_layout.rs`](./src/layout/block_layout.rs#L55) re-derives layout mode, float state, collapse state, and indentation behavior per child.
   - This is smaller than the table work, but still unnecessary repeated work.

6. Reduce style resolution work
   - [`apps/reader/crates/html/src/parser/html.rs`](./src/parser/html.rs#L122) still spends a lot of time in `resolve_styles_for_dom`.
   - The "do less" version here is to reduce how many nodes/selectors need matching, especially for nodes that cannot affect rendered output.

Suggested order:
1. Tables
2. Merge glyph post-passes
3. Anchor positions
4. Decorations
5. Block-flow cleanup
6. Style resolution

Memory usage reduction opportunities:

1. Shrink the glyph cache
   - [`apps/reader/crates/html/src/text/glyph_cache.rs`](./src/text/glyph_cache.rs#L63) stores a full `TextLayout` per cached glyph in `reverse`.
   - This is likely the largest retained heap in the renderer.

2. Remove the temporary line-sorting spike
   - [`apps/reader/crates/html/src/layout/engine.rs`](./src/layout/engine.rs#L150) drains `lines` and `line_glyph_offsets`, builds `line_pairs`, sorts them, then rebuilds both vectors.
   - That temporarily duplicates line data and offsets.

3. Compress `LayoutState`
   - [`apps/reader/crates/html/src/document/layout_state.rs`](./src/document/layout_state.rs#L8) keeps `lines`, `line_glyph_offsets`, `decorations`, `image_fragments`, `image_fragments_by_line`, and `anchor_positions`.
   - Some of these may be derivable instead of stored permanently.

4. Release build-time structures earlier
   - [`apps/reader/crates/html/src/document/loaded.rs`](./src/document/loaded.rs#L8) keeps `Document`, `LayoutTree`, `InlineContent`, `LayoutState`, and `GlyphCache` alive together.
   - If later phases no longer need some of these, they could be dropped sooner.

5. Reduce duplicated string storage
   - The DOM and navigation code still keep a lot of `String` values alive.
   - Likely candidates include DOM text, href/title strings, CSS chunks, and navigation history.

Cache locality opportunities:

1. Glyph cache layout
   - [`apps/reader/crates/html/src/text/glyph_cache.rs`](./src/text/glyph_cache.rs#L63) stores mixed-size data together in `reverse`.
   - Splitting hot metrics from the heavier `TextLayout` payload would improve locality.

2. Line sort/remap path
   - [`apps/reader/crates/html/src/layout/engine.rs`](./src/layout/engine.rs#L150) builds and sorts a tuple-heavy temporary.
   - Sorting indices and permuting backing vectors in place would be more cache-friendly.

3. Layout state fragmentation
   - [`apps/reader/crates/html/src/document/layout_state.rs`](./src/document/layout_state.rs#L8) spreads line-related data across several vectors and nested vectors.
   - A tighter packed representation would improve locality during rendering.

4. Inline layout pass structure
   - [`apps/reader/crates/html/src/layout/inline.rs`](./src/layout/inline.rs#L496) builds tokens, measures lines, and writes fragments in separate passes.
   - Fusing some of those passes would reduce rereads of the same token spans.

5. DOM/layout-tree access patterns
   - [`apps/reader/crates/html/src/document/loaded.rs`](./src/document/loaded.rs#L8) encourages bouncing across document, layout tree, inline content, and layout state.
   - Hot loops that repeatedly fetch style, parent, run, and line data from different structures can suffer extra cache misses.

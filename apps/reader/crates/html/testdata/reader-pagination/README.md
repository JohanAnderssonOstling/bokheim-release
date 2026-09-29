# Reader pagination visual checks

These fixtures exercise the book-reading layout path rather than browser-compatible continuous layout.

Regenerate the screenshots from `apps/reader/crates/html/` with:

```sh
cargo test -p html-view-gpui --test reader_pagination_visual -- --ignored --nocapture
```

The generated PNGs are written to `visual-artifacts/reader-pagination/`.

The large real-world table suite extracts all four tables from Appendices II
and III of the local *Bowling Alone* EPUB and renders every resulting page:

```sh
cargo test -p html-view-gpui --test bowling_alone_tables_visual -- --ignored --nocapture
```

Its PNGs are written to `visual-artifacts/bowling-alone-tables/`. By default
the test finds the EPUB in the repository root; set `BOWLING_ALONE_EPUB` to use
another location. The EPUB itself is not copied into the testdata directory.

Inspect them in filename order:

- `forced-break`: the purple chapter opening starts only on page 2.
- `avoid-inside`: the complete green card moves to page 2; its border is not split.
- `heading-keep`: the blue heading moves to page 2 with at least two lines of following prose.
- `figure-placement`: the complete orange figure and its caption move together to page 2.
- `table-placement`: the compact cyan table and caption move together to page 2.
- `table-row-groups`: the oversized table starts on page 2; its green row stays intact there, and both lavender rows connected by a rowspan move together to page 3.
- `widows-orphans`: the yellow paragraph moves intact to page 2 because page 1 cannot retain the required three lines.
- `popup-only-footnote`: the numbered link remains visible, but the magenta note body does not paint or consume space. Activating the link is covered by the separate footnote-preview interaction tests.

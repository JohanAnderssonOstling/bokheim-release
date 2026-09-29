# Local book inspection

This crate has no client, database, UI, metadata-server or network dependency.
Library ingestion (`book-metadata`) calls `inspect_epub` / `inspect_pdf` (or their seekable-reader equivalents).
Accepted local classifications and electronic ISBNs flow through their existing
metadata enrichment and subject projection paths.

## Bibliographic evidence

EPUB inspection checks the first and last eight spine documents, in reading order.
PDF inspection loads text dependencies for the first and last twenty pages, without
reading intervening page streams or image pixels. Both formats use the same
`evidence::inspect_section` rules. Optional malformed/oversized text sections are
skipped. XML head, script and style text cannot establish identity. Native builds also have a bounded Tesseract fallback; WASM remains text-only. See
[deployment and limits](../../docs/native-ocr-inspection.md).

A section must contain a copyright/CIP block near its beginning, or a structured
CIP continuation. Printed evidence has priority over damaged embedded
titles/authors. Bibliographies, indexes, advertisements and previews remain
excluded. Selected standalone ISBN images/back covers have a separate bounded
ISBN-only rule. ISBN checksums alone never establish ownership.

ISBNs retain explicit electronic, related-print or unspecified scope. Only an
unambiguous electronic ISBN compatible with the file format can fill a missing
book ISBN; existing identifiers are never replaced or supplemented by this
step. Explicitly labelled print ISBNs are automatically consumed by library rich
enrichment when LCC is missing. Unqualified ISBNs remain
review evidence. Neither scope is attached as book identity.

The network-free `related_isbn` module selects accepted print evidence, verifies
returned authority editions against the full local title and author credits, and
checks agreement among ISBN-to-LCC results. Callers use the existing metadata
service endpoints; only title/author queries and verified ISBNs leave the local
inspection process. Missing identity evidence, truncated searches, mismatching
volumes/authors, or conflicting LCC results fail closed. This currently requires
an Open Library edition identity match even when the ISBN's LCC comes from the
Library of Congress dump. LC-only records without verifiable identity remain for
review. Printed ISBNs that are absent from the bounded edition results are skipped.

Accepted codes carry `inspection:verified-print-isbn:<ISBN>` provenance. Only
classifications transfer, not print-edition identifiers, external work identity,
descriptions or entities. Existing ebook ISBNs remain intact; inspected books with
no ebook ISBN also participate in this fallback. Library enrichment provider v18
makes previous attempts eligible for retry.
LCC notation is accepted only from a matching CIP's classification field or a
standalone call-number line. Codes in illustration references are excluded.

Accepted evidence is recorded as a bounded `SectionEvidence` JSON value in
`BookProperty` (`inspection:bibliographic-evidence`, source
`inspection:local:v2`). This preserves source location, ISBN scope and codes for
either client or authority workflows. Generated subjects carry their page/section
source. Callers can also use the public evidence API on their own locally extracted
text; rejected sections include a reason.

Local EPUB/PDF import inspection cache versions were
advanced so older results are eligible for reinspection through the normal jobs.
This change does not itself start a library-wide rescan or deploy a service.

## Headless use and validation

```sh
cargo run --release -p book-inspection --example inspect_local -- book.epub book.pdf
cargo test --release -p book-inspection --lib
```

The example only reads files and prints accepted evidence. Regression fixtures
cover end-of-book CIP, advertisements, references, wrong volumes, collections,
missing authors, checksum errors, electronic/print/PDF scope, existing identifier
conflicts, separate-page isolation, malformed markup and bounded PDF reads.
Regression fixtures cover the same EPUB evidence paths.


PDF page evidence always uses PDFium. Run standalone PDF evidence probes with `cargo run --release -p book-inspection --example inspect_local -- <path>`.

PDFium opens a seekable reader on native and WASM, inspects bounded first/last pages, and uses the reader's character geometry to reconstruct visual lines. `lopdf` remains only for Info/XMP extraction; its text extraction is no longer used. Failure of that optional metadata extraction does not veto PDFium-readable page evidence. Source books remain unchanged.

Native PDF probes now require prepared PDFium inputs and a runtime package.
Run `python3 shared/pdfium/prepare.py --target x86_64-unknown-linux-gnu`
from the repository root before Cargo. For examples, also stage with
`python3 shared/pdfium/prepare.py --target x86_64-unknown-linux-gnu --dest target/release/examples/pdfium --offline`.
See `shared/pdfium/README.md` for other targets and offline archives.

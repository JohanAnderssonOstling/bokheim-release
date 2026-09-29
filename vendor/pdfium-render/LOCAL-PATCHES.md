# Local PDFium WASM patches (upstream 0.9.3)

- Enable `load_pdf_from_reader` on WASM and retain the reader for the document lifetime.
- Install the existing custom-read trampoline once for the PDFium instance. Readers are selected by their `m_Param`; the trampoline remains available after open for lazy reads and concurrent documents.
- Free the temporary PDFium file-access descriptor after opening; PDFium copies the descriptor, while the source reader remains owned by `PdfDocument`.
- Correct WASM `FPDFText_GetCharBox` argument order. This replaces the former UI-worker-only JavaScript workaround and also fixes geometry in inspection workers.

The application wraps Read with read_exact for PDFium callbacks. Browser regression uses a virtual 64 MiB PDF and a strict 1 MiB source-read budget, short reads, rendering, search, cancellation, repeated open/close and worker failures.

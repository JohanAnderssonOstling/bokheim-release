# PDFium dependency and packaging

`artifacts.toml` is the source of truth for supported targets, revisions, archive
URLs, checksums, and extracted files. Python 3.11+ is required. PDFium is never
compiled as part of the application build.

## Preparation

From the repository root:

```sh
python3 shared/pdfium/prepare.py --target x86_64-unknown-linux-gnu
```

Archives are cached by SHA-256 in `.pdfium-cache/`, outside Cargo's `target/`.
`PDFIUM_CACHE_DIR` or `--cache` selects a different cache. Prepared inputs live
in `.pdfium/<target>/`. `--dest` selects a staging directory. Repeated preparation
verifies archives and repairs modified outputs while preserving unchanged file
mtimes. Temporary downloads are verified before entering the cache.

For disconnected builds, supply a pinned archive once:

```sh
python3 shared/pdfium/prepare.py --target aarch64-linux-android \
  --archive /path/to/pdfium-android-arm64.tgz --offline
```

Local archives are also checksum verified and populate the cache. Later runs
can use `--offline` without `--archive`. WASM additionally needs the manifest's
explicit supplemental notices archive in the cache; prepare the Linux target
from its local archive first. The WASM supplier omits notices, so we preserve
the existing 7961 notices bundle and record that different provenance in VERSION.
WASM remains at 7902 pending validation of a newer Emscripten/bindings combination.

`build.rs` only validates prepared inputs against the manifest and receipt. It
does not download or extract anything. Missing/stale inputs report the exact
preparation command. `PDFIUM_PREPARED_DIR` overrides the prepared input directory.
Applications never use that build-machine directory at runtime (unit tests do).

## Platform entry points

- Desktop: `python3 apps/desktop-gpui/scripts/build.py` builds release and stages
  `target/release/pdfium/`. `--target` supports cross builds; `--offline` disables
  dependency downloads. Distribute the executable with its `pdfium/` directory on Linux; Windows links PDFium statically.
- AppImage: the packaging script stages PDFium and includes native dependencies.
- Windows x64 MSVC: PDFium 8046b and its /MT C runtime are statically linked.
  Both installer and update ZIP deliver the executable without a PDFium directory.
  The target-specific Cargo configuration enables the static CRT and selects
  `.pdfium/x86_64-pc-windows-msvc` for linking. Custom prepared directories must
  also set `PDFIUM_STATIC_LIB_PATH_x86_64_pc_windows_msvc` to the same directory.
  The pinned archive and library hashes are checked before use; notices from
  PdfReviewer are preserved in `windows-static/` with their source receipts.
  Windows has no PDFium runtime path override or temporary DLL extraction.
- Flatpak: prepare first, build with `--features flatpak`, and install the native
  library at `/app/lib/libpdfium.so`. `PDFIUM_PACKAGED_PATH` customizes this prefix.
- Metadata server: the remote build prepares and stages PDFium; server installers
  install and preserve it alongside the metadata executable. PDFium uses
  a GNU/Linux target because the prebuilt library
  requires glibc and dynamic loading. Other server targets remain independent.
- Android: Gradle prepares before Rust compilation and stages the shared library
  in the APK's ABI directory. The Android linker resolves it from the app package.
- Web: build and browser-test scripts prepare before compiling WASM and stage
  PDFium's JS/WASM files in the immutable bundle. The dedicated worker binds the
  separately instantiated PDFium module to the shared Rust PDF engine.

For other native tools that consume the PDF core, prepare before Cargo and stage
with `--dest <executable-directory>/pdfium`. For an example executable, that is
`target/release/examples/pdfium`. `BOKHEIM_PDFIUM_LIBRARY_PATH` overrides loading
for diagnostics and tests. Runtime extraction into temporary directories is gone.

## Updating and validation

Update the target entry's URL, revision, and verified checksum together. Prepare
again, then run the release PDF core tests and the browser worker tests. Native
and WASM runtime initialization is confined to the PDF core's `runtime/` modules;
reader, search, selection, and thumbnail consumers share the existing API.

```sh
python3 -m unittest discover -s shared/pdfium -p 'test_*.py'
cargo test --release -p pdf-reader-core --lib
bash apps/web-gpui/scripts/test-pdf-worker.sh
```

For Android, build `assembleRelease`, inspect APK native libraries and ELF
alignment, then test rendering/search/selection and restoring reading position
on a device. A successful cross compilation alone does not prove runtime loading.

The `pdf_runtime_probe` example opens a supplied PDF, renders every page, and
checks a supplied search string using the production loader. Build it with
`cargo build --release -p pdf-reader-core --example pdf_runtime_probe`.
For Android, cross-build with the NDK linker, then run the release executable
and matching prepared library from a temporary device directory with
`LD_LIBRARY_PATH` pointing there. This validates the Android ABI and engine
without changing the active document in the app.

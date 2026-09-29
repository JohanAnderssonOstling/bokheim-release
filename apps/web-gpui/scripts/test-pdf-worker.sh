#!/usr/bin/env bash
set -euo pipefail
app_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
workspace_dir="$(cd "$app_dir/../.." && pwd)"
python3 "$workspace_dir/shared/pdfium/prepare.py" --target wasm32-unknown-unknown
export CARGO_TARGET_DIR="${PDF_TEST_TARGET:-$workspace_dir/target/pdfium-browser-test}"
export PDF_TEST_DIST="${PDF_TEST_DIST:-$CARGO_TARGET_DIR/dist}"
cd "$workspace_dir"
CFLAGS_wasm32_unknown_unknown='-matomics -mbulk-memory' RUSTC_BOOTSTRAP=1 \
RUSTFLAGS='--cfg getrandom_backend="wasm_js" -C target-feature=+atomics,+bulk-memory,+mutable-globals -C link-arg=--shared-memory -C link-arg=--max-memory=2147483648 -C link-arg=--import-memory -C link-arg=--export=__wasm_init_tls -C link-arg=--export=__tls_size -C link-arg=--export=__tls_align -C link-arg=--export=__tls_base' \
cargo build --release -Z build-std=std,panic_abort -p pdf-reader-core --features browser-tests --example browser_pdf --target wasm32-unknown-unknown
mkdir -p "$PDF_TEST_DIST"
bindgen="${WASM_BINDGEN:-$workspace_dir/target/wasm-bindgen-cli/bin/wasm-bindgen}"
"$bindgen" --target web --out-dir "$PDF_TEST_DIST" --out-name web_gpui "$CARGO_TARGET_DIR/wasm32-unknown-unknown/release/examples/browser_pdf.wasm"
cp "$app_dir/pdf_worker.js" "$PDF_TEST_DIST/pdf_worker.js"
python3 "$app_dir/scripts/package-pdfium.py" "$PDF_TEST_DIST/pdfium"
node "$app_dir/scripts/pdf-worker.browser-test.mjs"

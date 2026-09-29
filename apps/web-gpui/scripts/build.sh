#!/usr/bin/env bash
set -euo pipefail

app_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
workspace_dir="$(cd "$app_dir/../.." && pwd)"
python3 "$workspace_dir/shared/pdfium/prepare.py" --target wasm32-unknown-unknown
output_dir="${BOKHEIM_WEB_OUTPUT_DIR:-$app_dir/dist}"
target_dir="${CARGO_TARGET_DIR:-$workspace_dir/target}"
if [[ "$target_dir" != /* ]]; then target_dir="$workspace_dir/$target_dir"; fi
local_bindgen="$workspace_dir/target/wasm-bindgen-cli/bin/wasm-bindgen"
bundle_dir="${output_dir}/pkg/.building"

(
    cd "$workspace_dir"
    RUSTC_BOOTSTRAP=1 cargo build \
        --manifest-path "$workspace_dir/Cargo.toml" \
        -p web-backend-worker \
        --target wasm32-unknown-unknown \
        --profile web-release
    RUSTC_BOOTSTRAP=1 cargo build \
        --manifest-path "$workspace_dir/Cargo.toml" \
        -p web-backend-worker --example cpu_worker --example sync_coordinator \
        --target wasm32-unknown-unknown --profile web-release
    # The UI's parser pool shares Rust-owned file handles and prepared documents.
    # Build Rust and C dependencies with atomics; backend workers stay isolated.
    CFLAGS_wasm32_unknown_unknown="${CFLAGS_wasm32_unknown_unknown:-} -matomics -mbulk-memory" \
    RUSTC_BOOTSTRAP=1 RUSTFLAGS='--cfg getrandom_backend="wasm_js" -C target-feature=+atomics,+bulk-memory,+mutable-globals -C link-arg=--shared-memory -C link-arg=--max-memory=2147483648 -C link-arg=--import-memory -C link-arg=--export=__wasm_init_tls -C link-arg=--export=__tls_size -C link-arg=--export=__tls_align -C link-arg=--export=__tls_base' cargo rustc \
        -Z build-std=std,panic_abort \
        --manifest-path "$workspace_dir/Cargo.toml" -p web-gpui \
        --target wasm32-unknown-unknown --profile web-release -- -C link-arg=--export=__heap_base
)
rm -rf -- "$output_dir"
mkdir -p "$bundle_dir"
install -d "$output_dir/assets/icons"
cp -a "$workspace_dir/vendor/gpui-component/crates/assets/assets/icons/." "$output_dir/assets/icons/"
if [[ -x "$local_bindgen" ]]; then
    wasm_bindgen="$local_bindgen"
else
    wasm_bindgen="$(command -v wasm-bindgen)"
fi
# Inline JS snippet names are local to each bindgen invocation. Keep outputs
# separate so one WASM module cannot overwrite another module's imports.
bind_module() {
    local module_name="$1"
    local module_binary="$2"
    "$wasm_bindgen" --target web --out-dir "$bundle_dir/$module_name" --out-name "$module_name" "$module_binary"
    printf 'import initialize from "./%s/%s.js";\nimport { compressedWasm } from "./compressed_wasm.js";\nexport * from "./%s/%s.js";\nexport default async function init(options) {\n  if (options !== undefined) return initialize(options);\n  const response = await compressedWasm(new URL("./%s/%s_bg.wasm.gz", import.meta.url));\n  return initialize(response ? { module_or_path: response } : undefined);\n}\n' "$module_name" "$module_name" "$module_name" "$module_name" "$module_name" "$module_name" > "$bundle_dir/$module_name.js"
}
bind_module web_gpui "$target_dir/wasm32-unknown-unknown/web-release/web_gpui.wasm"
bind_module web_backend_worker "$target_dir/wasm32-unknown-unknown/web-release/web_backend_worker.wasm"
bind_module cpu_worker "$target_dir/wasm32-unknown-unknown/web-release/examples/cpu_worker.wasm"
bind_module sync_coordinator "$target_dir/wasm32-unknown-unknown/web-release/examples/sync_coordinator.wasm"
cp "$app_dir/compressed_wasm.js" "$bundle_dir/compressed_wasm.js"
python3 - "$bundle_dir/sync_coordinator/sync_coordinator.js" <<'PY'
from pathlib import Path
import sys
module = Path(sys.argv[1]).read_text()
for name in ("startSharedCoordinator", "startNetworkWorker"):
    if f"export function {name}(" not in module:
        raise SystemExit(f"Coordinator WASM is missing required export {name}")
for name in ("synchronize", "backgroundCycle", "syncService", "cpuService", "startCoordinator"):
    if f"export function {name}(" in module:
        raise SystemExit(f"Coordinator WASM includes test-only export {name}")
PY
if command -v wasm-opt >/dev/null 2>&1; then
    for wasm in "$bundle_dir"/*/*_bg.wasm; do
        # Rust and wgpu emit post-MVP WebAssembly features such as SIMD and
        # bulk-memory operations. Binaryen validates inputs against MVP unless
        # their feature set is enabled explicitly.
        wasm-opt --all-features -Oz "$wasm" -o "${wasm}.optimized"
        mv "${wasm}.optimized" "$wasm"
    done
else
    echo "warning: wasm-opt is unavailable; shipping the size-optimized Rust output without Binaryen post-processing" >&2
fi

# A sidecar keeps the regular WASM path available for browsers without
# DecompressionStream while avoiding a full-size download on supported ones.
for wasm in "$bundle_dir"/*/*_bg.wasm; do
    gzip -9 -c "$wasm" > "${wasm}.gz"
done

# Some optimizers can leave an empty output after an exhausted filesystem.
# Reject incomplete artifacts before assigning an immutable bundle identity.
python3 - "$bundle_dir" <<'PY_VALIDATE_WASM'
from pathlib import Path
import sys
for module in Path(sys.argv[1]).glob("*/*_bg.wasm"):
    with module.open("rb") as source:
        if source.read(8) != b"\x00asm\x01\x00\x00\x00" or module.stat().st_size <= 8:
            raise SystemExit(f"Invalid or empty WebAssembly output: {module}")
PY_VALIDATE_WASM

cp "$workspace_dir/client/platforms/web/worker_transport.js" "$bundle_dir/worker_transport.js"
cp "$workspace_dir/client/platforms/web/import_io.js" "$bundle_dir/import_io.js"
cp "$workspace_dir/apps/web-backend-worker/web_backend_worker_loader.js" "$bundle_dir/web_backend_worker_loader.js"
cp "$workspace_dir/apps/web-backend-worker/web_backend_database_worker_loader.js" "$bundle_dir/web_backend_database_worker_loader.js"

for worker_source in coordinator.js cpu_worker_loader.js network_worker_loader.js worker_endpoint.js import_worker_loader.js import_worker.js; do
    cp "$workspace_dir/apps/web-backend-worker/$worker_source" "$bundle_dir/$worker_source"
done
# Source imports resolve within the repository; packaged workers share a bundle.
sed -i "s|../../client/platforms/web/import_io.js|./import_io.js|" "$bundle_dir/import_worker.js"


cp "$app_dir/pdf_worker.js" "$bundle_dir/pdf_worker.js"
python3 "$app_dir/scripts/package-pdfium.py" "$bundle_dir/pdfium"

bundle_hash=$(
    cd "$bundle_dir"
    find . -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum | sha256sum
)
build_id=${bundle_hash%% *}
build_id=${build_id:0:24}
asset_base="pkg/${build_id}"
mv "$bundle_dir" "$output_dir/${asset_base}"

cp "$workspace_dir/client/app/web/audiobook_service_worker.js" "$output_dir/audiobook_service_worker.js"
# This file is generated only after production signing certificates are known.
if [[ -f "$app_dir/assetlinks.json" ]]; then
    mkdir -p "$output_dir/.well-known"
    cp "$app_dir/assetlinks.json" "$output_dir/.well-known/assetlinks.json"
fi
cp "$app_dir/index.html" "$output_dir/index.html"
cp "$app_dir/compatibility.mjs" "$output_dir/compatibility.mjs"
cp "$app_dir/startup.mjs" "$output_dir/startup.mjs"
sed -i "s|\"\./pkg\"|\"./${asset_base}\"|" "$output_dir/index.html"
sed -i "s|\./pkg/web_gpui\.js|./${asset_base}/web_gpui.js|" "$output_dir/index.html"

# Compatibility entry for older clients. Current clients connect directly to
# the coordinator inside their immutable bundle.
printf 'import "./%s/web_backend_worker_loader.js";\n' "$asset_base" > "$output_dir/web_backend_worker_loader.js"

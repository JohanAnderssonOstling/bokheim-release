#!/usr/bin/env bash
set -euo pipefail

command -v brotli >/dev/null 2>&1 || { echo "error: install the brotli command to build the web release bundle" >&2; exit 1; }

app_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
workspace_dir="$(cd "$app_dir/../.." && pwd)"
python3 "$workspace_dir/shared/pdfium/prepare.py" --target wasm32-unknown-unknown
output_dir="${BOKHEIM_WEB_OUTPUT_DIR:-$app_dir/dist}"
target_dir="${CARGO_TARGET_DIR:-$workspace_dir/target}"
if [[ "$target_dir" != /* ]]; then target_dir="$workspace_dir/$target_dir"; fi
bundle_dir="${output_dir}/pkg/.building"
# Rebuild std for every module: Rust's distributed wasm std uses panic=abort.
# Use standardized exception instructions in both isolated and shared memory.
web_rustflags='--cfg getrandom_backend="wasm_js" -C panic=unwind -C target-feature=+exception-handling -C llvm-args=-wasm-use-legacy-eh=false'
provider_features=()
case "${BOKHEIM_GOOGLE_DRIVE:-false}" in
    true|1) provider_features=(--features google-drive) ;;
    false|0) ;;
    *) echo "error: BOKHEIM_GOOGLE_DRIVE must be true, false, 1, or 0" >&2; exit 2 ;;
esac


(
    cd "$workspace_dir"
    RUSTC_BOOTSTRAP=1 RUSTFLAGS="$web_rustflags" cargo build \
        -Z build-std=std,panic_unwind --no-default-features "${provider_features[@]}" \
        --manifest-path "$workspace_dir/Cargo.toml" \
        -p web-backend-worker \
        --target wasm32-unknown-unknown \
        --profile web-release
    RUSTC_BOOTSTRAP=1 RUSTFLAGS="$web_rustflags" cargo build \
        -Z build-std=std,panic_unwind --no-default-features "${provider_features[@]}" \
        --manifest-path "$workspace_dir/Cargo.toml" \
        -p web-backend-worker --example cpu_worker --example sync_coordinator \
        --target wasm32-unknown-unknown --profile web-release
    # The UI's parser pool shares Rust-owned file handles and prepared documents.
    # Build Rust and C dependencies with atomics; backend workers stay isolated.
    CFLAGS_wasm32_unknown_unknown="${CFLAGS_wasm32_unknown_unknown:-} -matomics -mbulk-memory" \
    RUSTC_BOOTSTRAP=1 RUSTFLAGS="$web_rustflags -C target-feature=+atomics,+bulk-memory,+mutable-globals,+exception-handling -C link-arg=--shared-memory -C link-arg=--max-memory=2147483648 -C link-arg=--import-memory -C link-arg=--export=__wasm_init_tls -C link-arg=--export=__tls_size -C link-arg=--export=__tls_align -C link-arg=--export=__tls_base" cargo rustc \
        -Z build-std=std,panic_unwind --no-default-features "${provider_features[@]}" \
        --manifest-path "$workspace_dir/Cargo.toml" -p web-gpui \
        --target wasm32-unknown-unknown --profile web-release -- -C link-arg=--export=__heap_base
)
rm -rf -- "$output_dir"
mkdir -p "$bundle_dir"
(
    cd "$workspace_dir"
    cargo run --release -p subject-projection --example export_bisac_reference -- "$bundle_dir"
    cargo run --release -p subject-projection --example export_web_taxonomy -- "$bundle_dir"
)
install -d "$output_dir/assets/icons"
cp -a "$workspace_dir/vendor/gpui-component/crates/assets/assets/icons/." "$output_dir/assets/icons/"
bindgen_version=$(python3 - "$workspace_dir/Cargo.lock" <<'PY_BINDGEN'
import sys, tomllib
with open(sys.argv[1], 'rb') as source:
    packages = tomllib.load(source)['package']
versions = {package['version'] for package in packages if package['name'] == 'wasm-bindgen'}
if len(versions) != 1:
    raise SystemExit('Expected one wasm-bindgen version in Cargo.lock')
print(versions.pop())
PY_BINDGEN
)
wasm_bindgen=
for candidate in "$workspace_dir/target/wasm-bindgen-cli-$bindgen_version/bin/wasm-bindgen" \
    "$workspace_dir/target/wasm-bindgen-cli/bin/wasm-bindgen" "$(command -v wasm-bindgen || true)"; do
    if [[ -x "$candidate" && "$("$candidate" --version)" == "wasm-bindgen $bindgen_version" ]]; then
        wasm_bindgen="$candidate"
        break
    fi
done
if [[ -z "$wasm_bindgen" ]]; then
    echo "error: install wasm-bindgen-cli $bindgen_version with --root $workspace_dir/target/wasm-bindgen-cli-$bindgen_version" >&2
    exit 1
fi
# Inline JS snippet names are local to each bindgen invocation. Keep outputs
# separate so one WASM module cannot overwrite another module's imports.
bind_module() {
    local module_name="$1"
    local module_binary="$2"
    # Release stripping removes target-feature metadata. Unwinding imports need
    # the externref transform even when bindgen cannot infer reference types.
    "$wasm_bindgen" --reference-types --target web --out-dir "$bundle_dir/$module_name" --out-name "$module_name" "$module_binary"
    printf 'import initialize from "./%s/%s.js";\nimport { compressedWasm } from "./compressed_wasm.js";\nexport * from "./%s/%s.js";\nexport default async function init(options) {\n  if (options !== undefined) return initialize(options);\n  const response = await compressedWasm(new URL("./%s/%s_bg.wasm", import.meta.url));\n  return initialize(response ? { module_or_path: response } : undefined);\n}\n' "$module_name" "$module_name" "$module_name" "$module_name" "$module_name" "$module_name" > "$bundle_dir/$module_name.js"
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
for name in ("startSharedCoordinator", "startNetworkWorker", "prepareBackend"):
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
cp "$workspace_dir/apps/web-backend-worker/web_backend_worker_loader.js" "$bundle_dir/web_backend_worker_loader.js"
cp "$workspace_dir/apps/web-backend-worker/web_backend_database_worker_loader.js" "$bundle_dir/web_backend_database_worker_loader.js"

for worker_source in coordinator.js cpu_worker_loader.js network_worker_loader.js worker_endpoint.js; do
    cp "$workspace_dir/apps/web-backend-worker/$worker_source" "$bundle_dir/$worker_source"
done


cp "$app_dir/pdf_worker.js" "$bundle_dir/pdf_worker.js"
python3 "$app_dir/scripts/package-pdfium.py" "$bundle_dir/pdfium"

# Reader-only glyph fallback is fetched through the existing font cache when
# a book opens; keep its bytes out of the initial UI WebAssembly module.
install -d "$bundle_dir/fonts"
cp "$workspace_dir/apps/desktop-gpui/assets/fonts/DroidSansFallback.ttf" "$workspace_dir/apps/desktop-gpui/assets/fonts/LICENSE-DROID.txt" "$bundle_dir/fonts/"

# HTTP Content-Encoding supports Brotli without a JavaScript decoder. Keep
# gzip and identity variants for clients and hosts with different capabilities.
python3 "$app_dir/scripts/compress-assets.py" "$bundle_dir"

bundle_hash=$(
    cd "$bundle_dir"
    find . -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum | sha256sum
)
build_id=${bundle_hash%% *}
build_id=${build_id:0:24}
asset_base="pkg/${build_id}"
mv "$bundle_dir" "$output_dir/${asset_base}"

cp "$workspace_dir/client/app/web/audiobook_service_worker.js" "$output_dir/audiobook_service_worker.js"
if (( ${#provider_features[@]} )); then
    cp "$workspace_dir/client/platforms/web/google_cloud_login.html" "$output_dir/google_cloud_login.html"
    cp "$workspace_dir/client/platforms/web/google_cloud_login.js" "$output_dir/google_cloud_login.js"
fi
# This file is generated only after production signing certificates are known.
if [[ -f "$app_dir/assetlinks.json" ]]; then
    mkdir -p "$output_dir/.well-known"
    cp "$app_dir/assetlinks.json" "$output_dir/.well-known/assetlinks.json"
fi
cp "$app_dir/index.html" "$output_dir/index.html"
cp "$app_dir/compatibility.mjs" "$output_dir/compatibility.mjs"
cp "$app_dir/startup.mjs" "$output_dir/startup.mjs"
cp "$app_dir/backend-startup.mjs" "$output_dir/backend-startup.mjs"
sed -i "s|\"\./pkg\"|\"./${asset_base}\"|" "$output_dir/index.html"
sed -i "s|\./pkg/web_gpui\.js|./${asset_base}/web_gpui.js|" "$output_dir/index.html"

# Compatibility entry for older clients. Current clients connect directly to
# the coordinator inside their immutable bundle.
printf 'import "./%s/web_backend_worker_loader.js";\n' "$asset_base" > "$output_dir/web_backend_worker_loader.js"

# These files refer to the final bundle identity, so compress them afterwards.
python3 "$app_dir/scripts/compress-assets.py" --top-level "$output_dir"

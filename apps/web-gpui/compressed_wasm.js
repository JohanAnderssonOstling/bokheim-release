// Fetch immutable WASM as gzip and restore the application/wasm response that
// wasm-bindgen needs for streaming compilation. Older browsers use the normal
// uncompressed module path.
export async function compressedWasm(url) {
    if (typeof DecompressionStream !== 'function') return null;
    let response;
    try {
        response = await fetch(url, {cache: 'force-cache'});
    } catch {
        return null;
    }
    if (!response.ok || !response.body) return null;
    return new Response(response.body.pipeThrough(new DecompressionStream('gzip')), {
        headers: {'Content-Type': 'application/wasm'},
    });
}

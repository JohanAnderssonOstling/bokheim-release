let cacheReady;

// Fetch through the existing service worker. A CacheStorage response delivered
// through fetch retains Chrome's compiled-WASM caching; direct cache.match does not.
async function prepareCache() {
    if (!globalThis.document || !globalThis.navigator?.serviceWorker) return;
    return cacheReady ??= (async () => {
        const workers = navigator.serviceWorker;
        await workers.register(new URL('../../audiobook_service_worker.js', import.meta.url));
        await workers.ready;
        if (!workers.controller) {
            await new Promise(resolve => workers.addEventListener('controllerchange', resolve, {once: true}));
        }
    })().catch(() => {});
}

export async function compressedWasm(url) {
    await prepareCache();
    let response;
    try {
        response = await fetch(url, {cache: 'force-cache'});
    } catch {
        return null;
    }
    if (!response.ok || !response.body) return null;
    return response;
}

// Start the Rust-owned tab transport using the small coordinator module while
// the much larger UI module downloads. Ownership moves to the UI exactly once.
export function prestartBackend(assetBase, {load = url => import(url)} = {}) {
    const brokerUrl = `${assetBase}/web_backend_worker_loader.js`;
    let claimed = false;
    let stopped = false;
    let prepared;
    const connection = (async () => {
        const module = await load(`${assetBase}/sync_coordinator.js`);
        if (stopped) throw new Error('Backend startup cancelled');
        await module.default();
        if (stopped) throw new Error('Backend startup cancelled');
        const result = await module.prepareBackend(brokerUrl);
        if (stopped && !claimed) {
            result.free();
            throw new Error('Backend startup cancelled');
        }
        prepared = result;
        return result;
    })();
    // UI initialization may still be downloading when early startup fails.
    // Preserve the rejection for take() without reporting it as unhandled.
    connection.catch(() => {});
    return {
        brokerUrl,
        take() {
            if (claimed || stopped) throw new Error('Backend startup is no longer available');
            claimed = true;
            return connection;
        },
        stop() {
            if (stopped) return;
            stopped = true;
            if (!claimed) prepared?.free();
        },
    };
}

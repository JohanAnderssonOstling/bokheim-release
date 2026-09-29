const RETRY_KEY = 'bokheim-startup-retry-v1';

export async function startApplication(panel, initialize, {reload = () => location.reload(), storage} = {}) {
    // Accessing sessionStorage itself can throw when browser storage is blocked.
    // In that case retain the manual Reload action without an automatic loop.
    if (storage === undefined) {
        try { storage = globalThis.sessionStorage; } catch { storage = null; }
    }
    panel.textContent = 'Opening Bokheim…';
    const ready = new Promise((resolve, reject) => {
        globalThis.__BOKHEIM_STARTUP_READY__ = resolve;
        globalThis.__BOKHEIM_STARTUP_FAILED__ = detail => reject(new Error(detail));
    });
    // A backend failure can arrive before the WASM initialization promise settles.
    ready.catch(() => {});
    try {
        await Promise.all([initialize(), ready]);
        try { storage.removeItem(RETRY_KEY); } catch { /* Storage may be disabled. */ }
        panel.remove();
        return true;
    } catch (error) {
        console.error('Bokheim startup failed', error);
        const busy = /BOKHEIM_STORAGE_BUSY|creating sync access handle/.test(String(error));
        let retry = false;
        if (!busy) {
            try {
                retry = storage.getItem(RETRY_KEY) !== '1';
                if (retry) storage.setItem(RETRY_KEY, '1');
            } catch { retry = false; }
        }
        if (retry) {
            panel.textContent = 'Retrying…';
            reload();
            return false;
        }
        const message = document.createElement('p');
        message.textContent = busy ? 'Close other Bokheim tabs, then reload.' : 'Could not update local data. Reload to try again.';
        const button = document.createElement('button');
        button.textContent = 'Reload';
        button.addEventListener('click', () => reload());
        panel.replaceChildren(message, button);
        return false;
    } finally {
        delete globalThis.__BOKHEIM_STARTUP_READY__;
        delete globalThis.__BOKHEIM_STARTUP_FAILED__;
    }
}

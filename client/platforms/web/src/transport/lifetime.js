// Browser lifetime primitives. Host selection and recovery policy live in Rust.
export function holdTabLock(name) {
    const abort = new AbortController();
    let releaseHeld;
    const ready = new Promise((acquired, failed) => {
        navigator.locks.request(name, {signal: abort.signal}, () => new Promise(release => {
            releaseHeld = release;
            acquired();
        })).catch(failed);
    });
    return {ready, release() {
        abort.abort();
        releaseHeld?.();
    }};
}

export function observeTabLock(name, released) {
    const abort = new AbortController();
    navigator.locks.request(name, {signal: abort.signal}, () => released()).catch(error => {
        if (error.name !== 'AbortError') released();
    });
    return () => abort.abort();
}

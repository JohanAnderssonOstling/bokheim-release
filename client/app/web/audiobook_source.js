// Audio bytes stay in the browser for local files. Remote requests use the
// backend's authenticated, bounded range reader; no credentials enter URLs.
const sources = new Map();
let registration;
let listening = false;

function abortable(work, signal) {
    return new Promise((resolve, reject) => {
        const aborted = () => { signal.removeEventListener('abort', aborted); reject(Error('Audiobook request cancelled')); };
        if (signal.aborted) {
            // The supplied operation may have synchronously triggered close.
            // Observe its eventual rejection even though cancellation wins.
            Promise.resolve(work).catch(() => {});
            aborted(); return;
        }
        signal.addEventListener('abort', aborted, {once: true});
        Promise.resolve(work).then(resolve, reject).finally(() => signal.removeEventListener('abort', aborted));
    });
}

function retryDelay(ms, signal) {
    return new Promise((resolve, reject) => {
        if (signal.aborted) { reject(Error('Audiobook request cancelled')); return; }
        const aborted = () => {
            clearTimeout(timer);
            signal.removeEventListener('abort', aborted);
            reject(Error('Audiobook request cancelled'));
        };
        const timer = setTimeout(() => {
            signal.removeEventListener('abort', aborted);
            resolve();
        }, ms);
        signal.addEventListener('abort', aborted, {once: true});
    });
}

export class AudioRangeRecovery {
    constructor(read, delay = retryDelay) {
        this.read = read;
        this.delay = delay;
        this.paused = false;
        this.resume = new Set();
        this.closed = new AbortController();
        this.failure = null;
    }
    setPaused(paused) {
        this.paused = paused;
        if (!paused) { for (const resume of this.resume) resume(); this.resume.clear(); }
    }
    close() { this.closed.abort(); this.read = null; }
    async range(offset, length, requestSignal) {
        const signal = AbortSignal.any([requestSignal, this.closed.signal]);
        for (let attempt = 0; attempt < 8; attempt++) {
            if (signal.aborted) throw Error('Audiobook request cancelled');
            if (this.failure) throw this.failure;
            // Initial metadata reads are allowed while paused; automatic retries
            // are gated until an explicit playback request resumes this source.
            if (attempt > 0) {
                await abortable(this.delay(Math.min(500 * 2 ** (attempt - 1), 8000), signal), signal);
                while (this.paused) {
                    let wake;
                    const resumed = new Promise(resolve => { wake = resolve; this.resume.add(wake); });
                    try { await abortable(resumed, signal); }
                    finally { this.resume.delete(wake); }
                }
            }
            if (signal.aborted) throw Error('Audiobook request cancelled');
            if (this.failure) throw this.failure;
            try {
                const bytes = await abortable(this.read(offset, length, signal), signal);
                if (!(bytes instanceof Uint8Array) || bytes.length !== length)
                    throw Object.assign(Error('Incomplete audiobook range'), {retryable: true});
                return bytes;
            } catch (error) {
                if (signal.aborted) throw error;
                if (!error?.retryable || attempt === 7) { this.failure = error; throw error; }
            }
        }
    }
}
export async function prepareRemoteAudio() {
    if (!navigator.serviceWorker) throw Error('This browser cannot stream audiobooks.');
    if (!registration) registration = (async () => {
        const script = new URL('audiobook_service_worker.js', document.baseURI);
        await navigator.serviceWorker.register(script.href, {scope: new URL('.', script).href});
        await navigator.serviceWorker.ready;
        if (navigator.serviceWorker.controller?.scriptURL !== script.href) await new Promise((resolve, reject) => {
            const changed = () => { if (navigator.serviceWorker.controller?.scriptURL === script.href) { clearTimeout(timer); navigator.serviceWorker.removeEventListener('controllerchange', changed); resolve(); } };
            const timer = setTimeout(() => { navigator.serviceWorker.removeEventListener('controllerchange', changed); reject(Error('Audio service worker did not activate')); }, 30000);
            navigator.serviceWorker.addEventListener('controllerchange', changed);
            changed();
        });
    })().catch(error => { registration = null; throw error; });
    await registration;
    if (!listening) {
        navigator.serviceWorker.addEventListener('message', event => {
            if (event.data?.type !== 'audiobook-range' || event.source !== navigator.serviceWorker.controller) return;
            const port = event.ports[0];
            if (!port) return;
            const source = sources.get(event.data.id);
            if (!source) { port.postMessage({error: 'Audiobook closed'}); port.close(); return; }
            source.ports.add(port);
            const cancelled = new AbortController();
            let busy = false;
            port.onmessage = async ({data}) => {
                if (data.close) { cancelled.abort(); source.ports.delete(port); port.close(); return; }
                let heartbeat;
                try {
                    if (busy || source.closed || !Number.isSafeInteger(data.offset) || data.offset < 0 || !Number.isInteger(data.length) || data.length <= 0 || data.length > 256 * 1024 || data.offset + data.length > source.length)
                        throw Error('Invalid audiobook range');
                    busy = true;
                    heartbeat = setInterval(() => port.postMessage({pending: true}), 10000);
                    const bytes = await source.recovery.range(data.offset, data.length, cancelled.signal);
                    if (!source.closed && !cancelled.signal.aborted) {
                        if (!(bytes instanceof Uint8Array) || bytes.length !== data.length) throw Error('Incomplete audiobook range');
                        port.postMessage({bytes}, [bytes.buffer]);
                    }
                } catch (error) { if (!cancelled.signal.aborted && !source.closed) port.postMessage({error: String(error)}); }
                finally { clearInterval(heartbeat); busy = false; }
            };
            port.postMessage({length: source.length});
        });
        listening = true;
    }
}
export function openRemoteAudio(length, read) {
    if (!Number.isSafeInteger(length) || length <= 0) throw Error('Invalid audiobook length');
    if (!listening) throw Error('Audio service worker has not been prepared');
    const id = crypto.randomUUID();
    const source = {id, length, recovery: new AudioRangeRecovery(read), ports: new Set(), closed: false};
    sources.set(id, source);
    source.url = new URL(`.bokheim-audio/${id}`, document.baseURI).href;
    return source;
}
export function closeRemoteAudio(source) {
    source.closed = true;
    source.recovery.close();
    sources.delete(source.id);
    for (const port of source.ports) { port.postMessage({error: 'Audiobook closed'}); port.close(); }
    source.ports.clear();
}
export function setRemoteAudioPaused(source, paused) { source.recovery.setPaused(paused); }
export function onAudioAbort(signal, callback) {
    if (signal.aborted) callback();
    else signal.addEventListener('abort', callback, {once: true});
}
export function offAudioAbort(signal, callback) { signal.removeEventListener('abort', callback); }

// Stateless media proxy: an existing tab owns every source and authenticates
// each bounded range. Restarting this worker requires no source re-registration.
self.addEventListener('install', event => event.waitUntil(self.skipWaiting()));
self.addEventListener('activate', event => event.waitUntil(self.clients.claim()));
self.addEventListener('fetch', event => {
    const url = new URL(event.request.url);
    const base = new URL('.bokheim-audio/', self.registration.scope);
    if (url.origin !== base.origin || !url.pathname.startsWith(base.pathname)) return;
    event.respondWith(audioResponse(event, url.pathname.slice(base.pathname.length)));
});
function range(header, size) {
    if (!header) return [0, size - 1, false];
    const match = /^bytes=(\d*)-(\d*)$/.exec(header);
    if (!match || (!match[1] && !match[2])) return null;
    let start, end;
    if (!match[1]) {
        const suffix = Number(match[2]);
        if (!Number.isSafeInteger(suffix) || suffix <= 0) return null;
        start = Math.max(0, size - suffix); end = size - 1;
    } else {
        start = Number(match[1]); end = match[2] ? Number(match[2]) : size - 1;
        if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || start >= size || end < start) return null;
        end = Math.min(end, size - 1);
    }
    return [start, end, true];
}
async function audioResponse(event, id) {
    if (!/^[0-9a-f-]{36}$/.test(id)) return new Response(null, {status: 404});
    if (!['GET', 'HEAD'].includes(event.request.method)) return new Response(null, {status: 405});
    const client = await self.clients.get(event.clientId);
    if (!client) return new Response('Audiobook tab unavailable', {status: 410});
    const channel = new MessageChannel();
    const port = channel.port1;
    let pending, closed = false;
    const next = () => new Promise((resolve, reject) => {
        const expired = () => { pending = null; reject(Error('Audiobook range timed out')); };
        let timer = setTimeout(expired, 35000);
        pending = {alive: () => { clearTimeout(timer); timer = setTimeout(expired, 35000); },
            resolve: data => { clearTimeout(timer); pending = null; resolve(data); }, reject: error => { clearTimeout(timer); pending = null; reject(error); }};
    });
    port.onmessage = ({data}) => {
        if (data.pending) { pending?.alive(); return; }
        if (data.error) pending?.reject(Error(data.error)); else pending?.resolve(data);
    };
    const close = () => {
        if (closed) return;
        closed = true;
        pending?.reject(Error('Audiobook request cancelled'));
        port.postMessage({close: true}); port.close();
    };
    try {
        const initial = next();
        client.postMessage({type: 'audiobook-range', id}, [channel.port2]);
        const {length: size} = await initial;
        if (!Number.isSafeInteger(size) || size <= 0) throw Error('Invalid audiobook length');
        const bounds = range(event.request.headers.get('Range'), size);
        if (!bounds) { close(); return new Response(null, {status: 416, headers: {'Content-Range': `bytes */${size}`}}); }
        let [position, end, partial] = bounds;
        const headers = {'Content-Type': 'audio/mp4', 'Accept-Ranges': 'bytes', 'Content-Length': String(end - position + 1), 'Cache-Control': 'no-store', 'Cross-Origin-Resource-Policy': 'same-origin'};
        if (partial) headers['Content-Range'] = `bytes ${position}-${end}/${size}`;
        if (event.request.method === 'HEAD') { close(); return new Response(null, {status: partial ? 206 : 200, headers}); }
        const body = new ReadableStream({
            async pull(controller) {
                try {
                    if (closed) return;
                    const count = Math.min(256 * 1024, end - position + 1);
                    const response = next();
                    port.postMessage({offset: position, length: count});
                    const {bytes} = await response;
                    if (closed) return;
                    if (!(bytes instanceof Uint8Array) || bytes.length !== count) throw Error('Incomplete audiobook range');
                    position += bytes.length;
                    controller.enqueue(bytes);
                    if (position > end) { controller.close(); close(); }
                } catch (error) { if (!closed) controller.error(error); close(); }
            },
            cancel() { close(); },
        }, {highWaterMark: 0});
        return new Response(body, {status: partial ? 206 : 200, headers});
    } catch (error) { close(); return new Response('Could not read audiobook', {status: 502}); }
}

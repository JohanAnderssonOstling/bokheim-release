import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {test} from 'node:test';
import {runInNewContext} from 'node:vm';

const source = readFileSync(new URL('../../client/app/web/audiobook_service_worker.js', import.meta.url), 'utf8');
const url = 'https://example.test/pkg/' + 'a'.repeat(24) + '/web_gpui/web_gpui_bg.wasm';
const wasm = () => new Response(new Uint8Array([0, 97, 115, 109, 1, 0, 0, 0]), {headers: {'Content-Type': 'application/wasm'}});

function worker(cache, fetch) {
    const listeners = {};
    runInNewContext(source, {URL, Request, Response, Headers,
        self: {location: new URL('https://example.test/audiobook_service_worker.js'),
            registration: {scope: 'https://example.test/'},
            addEventListener: (name, handler) => { listeners[name] = handler; }},
        caches: {open: async () => { if (cache instanceof Error) throw cache; return cache; }}, fetch,
    });
    return (address = url, method = 'GET') => {
        const pending = [];
        let response;
        listeners.fetch({request: new Request(address, {method}),
            respondWith: value => { response = value; }, waitUntil: value => pending.push(value)});
        return {get response() { return response; }, pending};
    };
}

test('a decoded WASM response is reused offline', async () => {
    const saved = wasm();
    const request = worker({match: async () => saved}, () => assert.fail('cache hit must not fetch'));
    assert.equal(await request().response, saved);
    assert.equal((await WebAssembly.instantiateStreaming(saved)).module instanceof WebAssembly.Module, true);
});

test('saving the decoded copy does not block the streaming response and removes only its older version', async () => {
    const previous = url.replace('a'.repeat(24), 'b'.repeat(24));
    const other = url.replaceAll('web_gpui', 'cpu_worker');
    const entries = new Map([[previous, wasm()], [other, wasm()]]);
    let release;
    const write = new Promise(resolve => { release = resolve; });
    const cache = {
        match: async key => entries.get(key.url),
        put: async (key, response) => { await write; entries.set(key.url, new Response(await response.arrayBuffer())); },
        keys: async () => [...entries.keys()].map(key => new Request(key)),
        delete: async key => entries.delete(key.url),
    };
    const network = wasm();
    const event = worker(cache, async () => network)();
    assert.equal(await event.response, network);
    assert.equal((await network.arrayBuffer()).byteLength, 8);
    release();
    await Promise.all(event.pending);
    assert.deepEqual([...entries.keys()].sort(), [other, url].sort());
});

test('disabled browser storage falls back to the ordinary fetch response', async () => {
    const network = wasm();
    assert.equal(await worker(Error('disabled'), async () => network)().response, network);
});

test('a quota failure does not fail application startup', async () => {
    const network = wasm();
    const event = worker({match: async () => undefined, put: async () => { throw Error('quota'); }}, async () => network)();
    assert.equal(await event.response, network);
    await Promise.all(event.pending);
    assert.equal((await WebAssembly.instantiateStreaming(network)).module instanceof WebAssembly.Module, true);
});

test('HTTP failures are not stored as applications', async () => {
    const network = new Response('unavailable', {status: 503});
    const event = worker({match: async () => undefined, put: () => assert.fail('must not cache an HTTP error')}, async () => network)();
    assert.equal(await event.response, network);
    assert.equal(event.pending.length, 0);
});

test('the WASM cache leaves unrelated requests untouched', () => {
    const request = worker({}, () => assert.fail('unrelated request intercepted'));
    for (const other of ['https://other.test'+new URL(url).pathname, 'https://example.test/index.html', url.replace('_bg.wasm', '.js')]) {
        assert.equal(request(other).response, undefined);
    }
    assert.equal(request(url, 'HEAD').response, undefined);
});

test('denied service-worker registration does not prevent the normal loader from starting', async t => {
    const {compressedWasm} = await import('./compressed_wasm.js');
    for (const [name, value] of Object.entries({document: {}, navigator: {
        serviceWorker: {register: async () => { throw Error('registration disabled'); }},
    }})) {
        const original = Object.getOwnPropertyDescriptor(globalThis, name);
        Object.defineProperty(globalThis, name, {configurable: true, value});
        t.after(() => {
            if (original) Object.defineProperty(globalThis, name, original);
            else delete globalThis[name];
        });
    }
    const network = wasm();
    t.mock.method(globalThis, 'fetch', async () => network);
    assert.equal(await compressedWasm(new URL(url)), network);
});

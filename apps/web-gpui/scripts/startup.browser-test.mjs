import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
const {firefox} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
const source = await readFile(new URL('../startup.mjs', import.meta.url));
const transport = await readFile(new URL('../../../client/platforms/web/worker_transport.js', import.meta.url));
const server = createServer((request, response) => {
    if (request.url === '/worker_transport.js') { response.setHeader('Content-Type', 'text/javascript'); response.end(transport); return; }
    if (request.url.startsWith('/worker.js')) { response.setHeader('Content-Type', 'text/javascript'); response.end("import {runWorkerEndpoint} from './worker_transport.js'; runWorkerEndpoint(() => self.postMessage({transport: 'backend_ready'}), 'bokheim-database-owner-v1').catch(() => {});"); return; }
    response.setHeader('Content-Type', request.url === '/startup.mjs' ? 'text/javascript' : 'text/html');
    response.end(request.url === '/startup.mjs' ? source : '<!doctype html><main id="startup-status"></main>');
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const browser = await firefox.launch({headless: true, executablePath: process.env.FIREFOX_PATH});
try {
    const page = await browser.newPage();
    await page.goto(`http://127.0.0.1:${server.address().port}`);
    const result = await page.evaluate(async () => {
        const {startApplication} = await import('/startup.mjs');
        let reloads = 0;
        const reload = () => { reloads++; };
        const panel = document.getElementById('startup-status');
        const fail = () => { globalThis.__BOKHEIM_STARTUP_FAILED__('migration failed'); };
        await startApplication(panel, fail, {reload});
        const first = {reloads, text: panel.textContent};
        await startApplication(panel, fail, {reload});
        const second = {reloads, text: panel.textContent, button: panel.querySelector('button')?.textContent};
        panel.querySelector('button').click();
        const clicked = reloads;
        await startApplication(panel, () => { globalThis.__BOKHEIM_STARTUP_FAILED__('BOKHEIM_STORAGE_BUSY'); }, {reload});
        const busy = {reloads, text: panel.textContent};
        await startApplication(panel, () => { globalThis.__BOKHEIM_STARTUP_READY__(); }, {reload});
        return {first, second, clicked, busy, removed: !panel.isConnected, reset: sessionStorage.getItem('bokheim-startup-retry-v1')};
    });
    assert.equal(result.first.reloads, 1);
    assert.equal(result.first.text, 'Retrying…');
    assert.equal(result.second.reloads, 1);
    assert.equal(result.second.button, 'Reload');
    assert.match(result.second.text, /Could not update local data/);
    assert.equal(result.clicked, 2);
    assert.equal(result.busy.reloads, 2);
    assert.match(result.busy.text, /Close other Bokheim tabs/);
    assert.equal(result.removed, true);
    assert.equal(result.reset, null);
    const blockedStorage = await page.evaluate(async () => {
        const {startApplication} = await import('/startup.mjs');
        const descriptor = Object.getOwnPropertyDescriptor(window, 'sessionStorage');
        Object.defineProperty(window, 'sessionStorage', {configurable: true, get() { throw new DOMException('Storage blocked', 'SecurityError'); }});
        const panel = document.createElement('main');
        document.body.append(panel);
        let reloads = 0;
        try {
            await startApplication(panel, () => { throw new Error('startup failed'); }, {reload: () => reloads++});
            return {reloads, button: panel.querySelector('button')?.textContent};
        } finally {
            Object.defineProperty(window, 'sessionStorage', descriptor);
            panel.remove();
        }
    });
    assert.equal(blockedStorage.reloads, 0);
    assert.equal(blockedStorage.button, 'Reload');
    const ownership = await page.evaluate(async () => {
        const start = async () => {
            const worker = new Worker('/worker.js?endpoint', {type: 'module'});
            const channel = new MessageChannel();
            const message = new Promise((resolve, reject) => {
                const timer = setTimeout(() => reject(new Error('worker ownership test timed out')), 10000);
                channel.port1.onmessage = event => { clearTimeout(timer); resolve(event.data); };
            });
            worker.postMessage({transport: 'attach_endpoint', port: channel.port2}, [channel.port2]);
            return {worker, port: channel.port1, message: await message};
        };
        const first = await start();
        const blocked = await start();
        const held = await navigator.locks.request('bokheim-database-owner-v1', {ifAvailable: true}, lock => !lock);
        blocked.worker.terminate(); blocked.port.close();
        first.worker.terminate(); first.port.close();
        await navigator.locks.request('bokheim-database-owner-v1', () => {});
        const next = await start();
        next.worker.terminate(); next.port.close();
        return {first: first.message, blocked: blocked.message, held, next: next.message};
    });
    assert.equal(ownership.first.transport, 'backend_ready');
    assert.equal(ownership.blocked.transport, 'endpoint_failed');
    assert.match(ownership.blocked.error, /BOKHEIM_STORAGE_BUSY/);
    assert.equal(ownership.held, true);
    assert.equal(ownership.next.transport, 'backend_ready');
    console.log('Browser startup tests passed: one automatic retry, persistent error, explicit reload, busy tabs and success.');
} finally { await browser.close(); await new Promise(resolve => server.close(resolve)); }

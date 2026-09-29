// Exercise the release bundle in Chromium while backend replies arrive after
// Settings opens. Uses Chromium's DevTools protocol without a test dependency.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {mkdtemp, readFile, rm, writeFile} from 'node:fs/promises';
import {createServer} from 'node:http';
import {tmpdir} from 'node:os';
import {join} from 'node:path';

const dist = new URL(process.env.BOKHEIM_TEST_DIST ? `file://${process.env.BOKHEIM_TEST_DIST.replace(/\/$/, '')}/` : '../dist/', import.meta.url);
let delivered = 0;
const server = createServer(async (request, response) => {
    response.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
    response.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
    response.setHeader('Cache-Control', 'no-store');
    const path = new URL(request.url, 'http://localhost').pathname;
    if (path === '/__test/delivered') {
        if (request.method === 'POST') delivered++;
        response.writeHead(200, {'Content-Type': 'text/plain'}).end(String(delivered));
        return;
    }
    try {
        const file = new URL(`.${path === '/' ? '/index.html' : path}`, dist);
        if (!file.href.startsWith(dist.href)) throw Error('Invalid path');
        let bytes = await readFile(file);
        if (path.endsWith('/web_backend_database_worker_loader.js')) {
            // The worker sends encoded RPC replies through MessagePort. Delay
            // them beyond the first Settings click, and count actual delivery.
            const delay = `
                const originalPostMessage = MessagePort.prototype.postMessage;
                MessagePort.prototype.postMessage = function (message, transfer) {
                    if (!(message instanceof Uint8Array) && !(message instanceof ArrayBuffer))
                        return originalPostMessage.call(this, message, transfer);
                    setTimeout(() => {
                        originalPostMessage.call(this, message, transfer);
                        fetch('/__test/delivered', {method: 'POST'}).catch(() => {});
                    }, 5000);
                };
            `;
            bytes = Buffer.concat([Buffer.from(delay), bytes]);
        }
        response.setHeader('Content-Type', path.endsWith('.wasm') ? 'application/wasm' : /\.(m?js)$/.test(path) ? 'text/javascript' : path === '/' ? 'text/html' : 'application/octet-stream');
        response.writeHead(200).end(bytes);
    } catch (error) {
        response.writeHead(404).end(error.message);
    }
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));

const profile = await mkdtemp(join(tmpdir(), 'bokheim-settings-async-'));
const browser = spawn(process.env.CHROMIUM_PATH || '/usr/bin/chromium', [
    '--headless=new', '--no-sandbox', '--disable-dev-shm-usage',
    '--enable-unsafe-webgpu', '--use-angle=swiftshader', '--enable-features=Vulkan,WebGPUDeveloperFeatures',
    `--user-data-dir=${profile}`, '--remote-debugging-port=0', 'about:blank',
], {stdio: 'ignore'});

const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
async function until(check, timeout = 90000) {
    const deadline = Date.now() + timeout;
    while (Date.now() < deadline) {
        const result = await check();
        if (result) return result;
        await pause(100);
    }
    throw Error('Timed out waiting for Chromium or the application');
}

let socket;
try {
    const port = await until(async () => {
        if (browser.exitCode !== null) throw Error(`Chromium exited with ${browser.exitCode}`);
        try { return Number((await readFile(join(profile, 'DevToolsActivePort'), 'utf8')).split('\n')[0]); }
        catch { return 0; }
    }, 15000);
    const target = await until(async () => {
        try { return (await (await fetch(`http://127.0.0.1:${port}/json/list`)).json()).find(entry => entry.type === 'page'); }
        catch { return null; }
    });
    socket = new WebSocket(target.webSocketDebuggerUrl);
    await new Promise((resolve, reject) => { socket.addEventListener('open', resolve, {once: true}); socket.addEventListener('error', reject, {once: true}); });
    let id = 0;
    const pending = new Map();
    const errors = [];
    socket.addEventListener('message', ({data}) => {
        const message = JSON.parse(data);
        if (message.id) {
            const waiter = pending.get(message.id);
            pending.delete(message.id);
            if (message.error) waiter.reject(Error(message.error.message)); else waiter.resolve(message.result);
        }
        if (message.method === 'Runtime.exceptionThrown') errors.push(message.params.exceptionDetails.text);
        if (message.method === 'Runtime.consoleAPICalled') {
            errors.push(message.params.args.map(arg => arg.value || arg.description || '').join(' '));
        }
    });
    const send = (method, params = {}) => new Promise((resolve, reject) => {
        const requestId = ++id;
        pending.set(requestId, {resolve, reject});
        socket.send(JSON.stringify({id: requestId, method, params}));
    });
    const evaluate = async expression => (await send('Runtime.evaluate', {expression, returnByValue: true, awaitPromise: true})).result.value;
    const click = async (x, y) => {
        await send('Input.dispatchMouseEvent', {type: 'mousePressed', x, y, button: 'left', clickCount: 1});
        await send('Input.dispatchMouseEvent', {type: 'mouseReleased', x, y, button: 'left', clickCount: 1});
    };
    await send('Runtime.enable');
    await send('Page.enable');
    await send('Emulation.setDeviceMetricsOverride', {width: 1100, height: 850, deviceScaleFactor: 1, mobile: false});
    await send('Page.navigate', {url: `http://127.0.0.1:${server.address().port}/`});
    await until(async () => {
        try { return await evaluate('Boolean(document.querySelector("canvas"))'); }
        catch { return false; }
    });
    await pause(1500);
    const before = delivered;
    await click(40, 485);
    await until(() => delivered > before, 30000);
    await pause(1800);
    await click(824, 43);
    const dialogOpen = await evaluate('Boolean(document.querySelector("dialog"))');
    if (!dialogOpen) {
        const screenshot = await send('Page.captureScreenshot', {format: 'png'});
        await writeFile('/tmp/bokheim-settings-async-failure.png', Buffer.from(screenshot.data, 'base64'));
        console.error(`Delayed replies after Settings click: ${delivered - before}`);
        console.error(errors.slice(-10).join('\n'));
    }
    assert.equal(dialogOpen, true, 'Settings must remain interactive after worker results');
    assert.equal(errors.filter(error => /RefCell already borrowed|already borrowed|panicked at/.test(error)).length, 0, errors.join('\n'));
    console.log(`PASS: Chromium Settings stayed responsive across ${delivered - before} delayed worker replies`);
} finally {
    socket?.close();
    browser.kill();
    server.close();
    server.closeAllConnections();
    await rm(profile, {recursive: true, force: true});
}

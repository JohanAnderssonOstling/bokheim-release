// Import a small EPUB through Chromium's file or folder picker.
// The picker token must survive until the import worker takes the File.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {copyFile, mkdir, mkdtemp, readFile, rm, writeFile} from 'node:fs/promises';
import {createServer} from 'node:http';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {fileURLToPath} from 'node:url';

const dist = new URL(process.env.BOKHEIM_TEST_DIST ? `file://${process.env.BOKHEIM_TEST_DIST.replace(/\/$/, '')}/` : '../dist/', import.meta.url);
const fixture = fileURLToPath(new URL('../../../client/app/tests/fixtures/storage-contract.epub', import.meta.url));
const server = createServer(async (request, response) => {
    response.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
    response.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
    response.setHeader('Cache-Control', 'no-store');
    const path = new URL(request.url, 'http://localhost').pathname;
    try {
        const file = new URL(`.${path === '/' ? '/index.html' : path}`, dist);
        if (!file.href.startsWith(dist.href)) throw Error('Invalid path');
        const bytes = await readFile(file);
        response.setHeader('Content-Type', path.endsWith('.wasm') ? 'application/wasm' : /\.(m?js)$/.test(path) ? 'text/javascript' : path === '/' ? 'text/html' : 'application/octet-stream');
        response.writeHead(200).end(bytes);
    } catch (error) {
        response.writeHead(404).end(error.message);
    }
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));

const profile = await mkdtemp(join(tmpdir(), 'bokheim-file-import-'));
const folderMode = Boolean(process.env.BOKHEIM_TEST_LIBRARY_FOLDER);
const selectedFolder = join(profile, 'Selected Library');
if (folderMode) {
    await mkdir(selectedFolder);
    await copyFile(fixture, join(selectedFolder, 'Book.epub'));
}
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
    throw Error('Timed out waiting for Chromium or the file picker');
}

let socket;
let browserSocket;
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
    const allWorkerLogs = [];
    if (process.env.BOKHEIM_TEST_VERBOSE) {
        const version = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json();
        browserSocket = new WebSocket(version.webSocketDebuggerUrl);
        await new Promise(resolve => browserSocket.addEventListener('open', resolve, {once: true}));
        let browserId = 0;
        const browserPending = new Map();
        const browserSend = (method, params = {}, sessionId) => new Promise((resolve, reject) => {
            const requestId = ++browserId;
            browserPending.set(requestId, {resolve, reject});
            browserSocket.send(JSON.stringify({id: requestId, method, params, ...(sessionId ? {sessionId} : {})}));
        });
        const sessionTypes = new Map();
        browserSocket.addEventListener('message', ({data}) => {
            const message = JSON.parse(data);
            if (message.id) {
                const waiter = browserPending.get(message.id);
                browserPending.delete(message.id);
                if (message.error) waiter?.reject(Error(message.error.message)); else waiter?.resolve(message.result);
            }
            if (message.method === 'Target.attachedToTarget') {
                const {sessionId, targetInfo} = message.params;
                sessionTypes.set(sessionId, targetInfo.type);
                browserSend('Runtime.enable', {}, sessionId).catch(error => allWorkerLogs.push(String(error)));
                if (targetInfo.type === 'page' || targetInfo.type === 'shared_worker') {
                    browserSend('Target.setAutoAttach', {autoAttach: true, waitForDebuggerOnStart: false, flatten: true}, sessionId).catch(() => {});
                }
            }
            if (message.sessionId && message.method === 'Runtime.consoleAPICalled') {
                const line = message.params.args.map(arg => arg.value || arg.description || '').join(' ');
                if (message.params.type !== 'info' || /Restarting browser database/.test(line)) allWorkerLogs.push(`${sessionTypes.get(message.sessionId)} ${line}`);
            }
            if (message.sessionId && message.method === 'Runtime.exceptionThrown') allWorkerLogs.push(`${sessionTypes.get(message.sessionId)} ${message.params.exceptionDetails.exception?.description || message.params.exceptionDetails.text}`);
        });
        await browserSend('Target.setAutoAttach', {autoAttach: true, waitForDebuggerOnStart: false, flatten: true});
    }
    socket = new WebSocket(target.webSocketDebuggerUrl);
    await new Promise((resolve, reject) => { socket.addEventListener('open', resolve, {once: true}); socket.addEventListener('error', reject, {once: true}); });
    let id = 0;
    let chooser;
    const pending = new Map();
    const errors = [];
    const workerLogs = [];
    const workers = new Map();
    socket.addEventListener('message', ({data}) => {
        const message = JSON.parse(data);
        if (message.id) {
            const waiter = pending.get(message.id);
            pending.delete(message.id);
            if (message.error) waiter.reject(Error(message.error.message)); else waiter.resolve(message.result);
        }
        if (message.method === 'Page.fileChooserOpened') chooser = message.params;
        if (message.method === 'Target.targetCreated' && /worker/.test(message.params.targetInfo.type)) {
            const target = message.params.targetInfo;
            send('Target.attachToTarget', {targetId: target.targetId, flatten: true}).catch(error => workerLogs.push(String(error)));
        }
        if (message.method === 'Target.attachedToTarget') {
            const {sessionId, targetInfo} = message.params;
            workers.set(sessionId, targetInfo.type);
            send('Runtime.enable', {}, sessionId).catch(error => workerLogs.push(String(error)));
        }
        if (message.sessionId && message.method === 'Runtime.consoleAPICalled') {
            const line = message.params.args.map(arg => arg.value || arg.description || '').join(' ');
            if (message.params.type === 'error' || message.params.type === 'warning' || /Restarting browser database/.test(line)) workerLogs.push(`${workers.get(message.sessionId)} ${line}`);
        }
        if (message.sessionId && message.method === 'Runtime.exceptionThrown') workerLogs.push(`${workers.get(message.sessionId)} ${message.params.exceptionDetails.exception?.description || message.params.exceptionDetails.text}`);
        if (message.method === 'Runtime.exceptionThrown') {
            const detail = message.params.exceptionDetails;
            errors.push([detail.text, detail.exception?.description].filter(Boolean).join(': '));
        }
        if (message.method === 'Runtime.consoleAPICalled' && message.params.type === 'error') {
            errors.push(message.params.args.map(arg => arg.value || arg.description || '').join(' '));
        }
    });
    const send = (method, params = {}, sessionId) => new Promise((resolve, reject) => {
        const requestId = ++id;
        pending.set(requestId, {resolve, reject});
        socket.send(JSON.stringify({id: requestId, method, params, ...(sessionId ? {sessionId} : {})}));
    });
    const evaluate = async expression => (await send('Runtime.evaluate', {expression, returnByValue: true, awaitPromise: true})).result.value;
    await send('Runtime.enable');
    await send('Page.enable');
    await send('DOM.enable');
    await send('Target.setDiscoverTargets', {discover: true});
    await send('Page.addScriptToEvaluateOnNewDocument', {source: `
        globalThis.__importPortEvents = [];
        const start = MessagePort.prototype.start;
        MessagePort.prototype.start = function (...args) {
            this.addEventListener('message', ({data}) => {
                if (data?.transport?.startsWith?.('import_') || data?.kind === 'reconnecting')
                    globalThis.__importPortEvents.push({transport: data.transport, kind: data.kind, phase: data.phase, error: data.error, processed: data.processed, totalFiles: data.totalFiles, failed: data.failed, failures: data.failures});
            });
            return start.apply(this, args);
        };
    `});
    await send('Page.setInterceptFileChooserDialog', {enabled: true});
    await send('Emulation.setDeviceMetricsOverride', {width: 1100, height: 850, deviceScaleFactor: 1, mobile: false});
    await send('Page.navigate', {url: `http://127.0.0.1:${server.address().port}/`});
    await until(async () => {
        try { return await evaluate('Boolean(document.querySelector("canvas"))'); }
        catch { return false; }
    });
    await pause(3000);
    if (folderMode) {
        await send('Input.dispatchMouseEvent', {type: 'mousePressed', x: 510, y: 453, button: 'left', clickCount: 1});
        await send('Input.dispatchMouseEvent', {type: 'mouseReleased', x: 510, y: 453, button: 'left', clickCount: 1});
        await until(() => chooser, 10000);
        await send('DOM.setFileInputFiles', {backendNodeId: chooser.backendNodeId, files: [selectedFolder]});
        chooser = null;
        await pause(2000);
        await send('Input.dispatchMouseEvent', {type: 'mousePressed', x: 395, y: 463, button: 'left', clickCount: 1});
        await send('Input.dispatchMouseEvent', {type: 'mouseReleased', x: 395, y: 463, button: 'left', clickCount: 1});
        await send('Input.dispatchMouseEvent', {type: 'mousePressed', x: 682, y: 502, button: 'left', clickCount: 1});
        await send('Input.dispatchMouseEvent', {type: 'mouseReleased', x: 682, y: 502, button: 'left', clickCount: 1});
    } else {
        await send('Input.dispatchMouseEvent', {type: 'mousePressed', x: 660, y: 453, button: 'left', clickCount: 1});
        await send('Input.dispatchMouseEvent', {type: 'mouseReleased', x: 660, y: 453, button: 'left', clickCount: 1});
        await pause(500);
        await send('Input.dispatchMouseEvent', {type: 'mousePressed', x: 465, y: 389, button: 'left', clickCount: 1});
        await send('Input.dispatchMouseEvent', {type: 'mouseReleased', x: 465, y: 389, button: 'left', clickCount: 1});
        for (const character of 'Import regression') {
            const code = character === ' ' ? 'Space' : `Key${character.toUpperCase()}`;
            await send('Input.dispatchKeyEvent', {type: 'keyDown', key: character, code, text: character});
            await send('Input.dispatchKeyEvent', {type: 'keyUp', key: character, code});
        }
        await send('Input.dispatchMouseEvent', {type: 'mousePressed', x: 395, y: 463, button: 'left', clickCount: 1});
        await send('Input.dispatchMouseEvent', {type: 'mouseReleased', x: 395, y: 463, button: 'left', clickCount: 1});
        await send('Input.dispatchMouseEvent', {type: 'mousePressed', x: 682, y: 502, button: 'left', clickCount: 1});
        await send('Input.dispatchMouseEvent', {type: 'mouseReleased', x: 682, y: 502, button: 'left', clickCount: 1});
        await pause(2500);
        if (process.env.BOKHEIM_TEST_CREATE_SCREENSHOT) {
            const screenshot = await send('Page.captureScreenshot', {format: 'png'});
            await writeFile(process.env.BOKHEIM_TEST_CREATE_SCREENSHOT, Buffer.from(screenshot.data, 'base64'));
        }
        await send('Input.dispatchMouseEvent', {type: 'mousePressed', x: 590, y: 470, button: 'left', clickCount: 1});
        await send('Input.dispatchMouseEvent', {type: 'mouseReleased', x: 590, y: 470, button: 'left', clickCount: 1});
        await until(() => chooser, 10000);
        await send('DOM.setFileInputFiles', {backendNodeId: chooser.backendNodeId, files: [fixture]});
    }
    const settled = await until(async () => {
        const events = await evaluate('globalThis.__importPortEvents || []');
        return events.find(event => event.transport === 'import_progress' && event.phase === 'complete') ||
            events.find(event => event.kind === 'reconnecting');
    }, Number(process.env.BOKHEIM_TEST_SETTLE_MS || 60000)).catch(() => null);
    if (process.env.BOKHEIM_TEST_RELOAD_AFTER) {
        await send('Page.reload', {ignoreCache: true});
        await until(async () => {
            try { return await evaluate('Boolean(document.querySelector("canvas"))'); }
            catch { return false; }
        });
        await pause(Number(process.env.BOKHEIM_TEST_RELOAD_PAUSE_MS || 8000));
    }
    const relevant = errors.filter(error => /Selected file is no longer available|panicked at|RefCell already borrowed|Uncaught Error/.test(error));
    const importEvents = await evaluate('globalThis.__importPortEvents || []');
    if (relevant.length || process.env.BOKHEIM_TEST_SCREENSHOT) {
        const screenshot = await send('Page.captureScreenshot', {format: 'png'});
        await writeFile(process.env.BOKHEIM_TEST_SCREENSHOT || '/tmp/bokheim-file-import-failure.png', Buffer.from(screenshot.data, 'base64'));
    }
    if (process.env.BOKHEIM_TEST_SETTINGS_SCREENSHOT) {
        await send('Input.dispatchMouseEvent', {type: 'mousePressed', x: 40, y: 485, button: 'left', clickCount: 1});
        await send('Input.dispatchMouseEvent', {type: 'mouseReleased', x: 40, y: 485, button: 'left', clickCount: 1});
        await pause(Number(process.env.BOKHEIM_TEST_SETTINGS_WAIT_MS || 2000));
        const screenshot = await send('Page.captureScreenshot', {format: 'png'});
        await writeFile(process.env.BOKHEIM_TEST_SETTINGS_SCREENSHOT, Buffer.from(screenshot.data, 'base64'));
    }
    if (process.env.BOKHEIM_TEST_VERBOSE) {
        const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
        const shared = targets.find(target => target.type === 'shared_worker');
        if (shared?.webSocketDebuggerUrl) {
            const workerSocket = new WebSocket(shared.webSocketDebuggerUrl);
            await new Promise(resolve => workerSocket.addEventListener('open', resolve, {once: true}));
            workerSocket.addEventListener('message', ({data}) => {
                const message = JSON.parse(data);
                if (message.method === 'Runtime.consoleAPICalled') workerLogs.push(message.params.args.map(arg => arg.value || arg.description || '').join(' '));
                if (message.method === 'Runtime.exceptionThrown') workerLogs.push(message.params.exceptionDetails.exception?.description || message.params.exceptionDetails.text);
            });
            workerSocket.send(JSON.stringify({id: 1, method: 'Runtime.enable'}));
            await pause(3000);
            workerSocket.close();
        }
        console.log(JSON.stringify({settled, importEvents: importEvents.slice(-20), errors: errors.slice(0, 8), workerLogs: workerLogs.slice(0, 8), allWorkerLogs: allWorkerLogs.slice(0, 8)}));
    }
    assert.deepEqual(relevant, [], `Browser import errors:\n${errors.join('\n')}`);
    assert.equal(importEvents.some(event => event.kind === 'reconnecting'), false, `Database worker reconnected during import: ${JSON.stringify(importEvents.slice(-10))}`);
    assert.ok(settled?.phase === 'complete', `Import did not complete: ${JSON.stringify(importEvents.slice(-10))}`);
    assert.equal(settled.processed, 1, `Import completed without processing the selected EPUB: ${JSON.stringify(settled)}`);
    assert.equal(settled.failed, 0, `Import completed with a failed file: ${JSON.stringify(settled)}`);
    assert.deepEqual(settled.failures, [], `Import completed with failures: ${JSON.stringify(settled)}`);
    console.log(`PASS: Chromium imported one EPUB from ${folderMode ? 'a folder' : 'the file picker'} without losing its file token or reconnecting the database worker`);
} finally {
    socket?.close();
    browserSocket?.close();
    browser.kill();
    server.close();
    server.closeAllConnections();
    if (browser.exitCode === null) await new Promise(resolve => browser.once('exit', resolve));
    for (let attempt = 0; attempt < 10; attempt++) {
        try {
            await rm(profile, {recursive: true, force: true});
            break;
        } catch (error) {
            if (error.code !== 'ENOTEMPTY' || attempt === 9) throw error;
            await pause(100);
        }
    }
}

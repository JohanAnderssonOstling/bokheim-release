import assert from 'node:assert/strict';
import {execFile} from 'node:child_process';
import {promisify} from 'node:util';
import {createServer} from 'node:http';
import {mkdtemp, readFile, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join, resolve, sep} from 'node:path';
import {fileURLToPath} from 'node:url';
const run = promisify(execFile);
const {firefox} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
const workspace = fileURLToPath(new URL('../../../', import.meta.url));
const temporary = await mkdtemp(join(tmpdir(), 'bokheim-update-gate-'));
let browser, server;
try {
    await run('cargo', ['build', '--release', '-p', 'client-platform-runtime', '--features', 'web-runtime-tests', '--example', 'startup_gate_browser', '--target', 'wasm32-unknown-unknown'], {cwd: workspace});
    const target = resolve(workspace, process.env.CARGO_TARGET_DIR || 'target');
    await run(process.env.WASM_BINDGEN || join(workspace, 'target/wasm-bindgen-cli/bin/wasm-bindgen'), ['--target', 'web', '--out-dir', temporary, '--out-name', 'gate', join(target, 'wasm32-unknown-unknown/release/examples/startup_gate_browser.wasm')]);
    server = createServer(async (request, response) => {
        try {
            const url = new URL(request.url, 'http://localhost');
            const file = resolve(temporary, url.pathname.slice(1));
            if (!file.startsWith(temporary + sep)) { response.end('<!doctype html><title>Startup gate fixture</title>'); return; }
            response.setHeader('Content-Type', url.pathname.endsWith('.wasm') ? 'application/wasm' : 'text/javascript');
            if (url.pathname === '/worker.js') {
                response.end('import init, {run_contract} from "./gate.js"; await init(); postMessage(await run_contract(new URL(self.location.href).searchParams.has("abandon")));');
            } else response.end(await readFile(file));
        } catch { response.writeHead(404); response.end(); }
    });
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
    browser = await firefox.launch({headless: true, executablePath: process.env.FIREFOX_PATH});
    const page = await browser.newPage();
    await page.goto(`http://127.0.0.1:${server.address().port}`);
    for (const abandon of [false, true]) {
        const passed = await page.evaluate(abandon => new Promise((resolve, reject) => {
            const worker = new Worker('/worker.js' + (abandon ? '?abandon' : ''), {type: 'module'});
            const timer = setTimeout(() => { worker.terminate(); reject(new Error('startup gate timed out')); }, 10000);
            worker.onmessage = event => { clearTimeout(timer); worker.terminate(); resolve(event.data); };
            worker.onerror = event => { clearTimeout(timer); worker.terminate(); reject(new Error(event.message)); };
        }), abandon);
        assert.equal(passed, true);
    }
    console.log('Browser update gate tests passed: background exclusion, explicit release, abandoned trial.');
} finally {
    if (browser) await browser.close();
    if (server) await new Promise(resolve => server.close(resolve));
    await rm(temporary, {recursive: true, force: true});
}

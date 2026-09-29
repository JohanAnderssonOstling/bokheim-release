import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {readFile, writeFile} from 'node:fs/promises';
import {resolve, extname} from 'node:path';
const {chromium} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');

import {fixturePdf} from './pdf-fixture.mjs';

const root = resolve(process.env.PDF_TEST_DIST);
await writeFile(resolve(root, 'fixture.pdf'), fixturePdf());
const server = createServer(async (request, response) => {
    response.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
    response.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
    const pathname = new URL(request.url, 'http://localhost').pathname;
    if (pathname === '/') { response.setHeader('Content-Type', 'text/html'); response.end('<!doctype html><title>PDF worker test</title>'); return; }
    const path = resolve(root, `.${pathname}`);
    if (!path.startsWith(`${root}/`)) { response.writeHead(403).end(); return; }
    try {
        response.setHeader('Content-Type', {'.js': 'text/javascript', '.wasm': 'application/wasm', '.pdf': 'application/pdf'}[extname(path)] || 'application/octet-stream');
        response.end(await readFile(path));
    } catch { response.writeHead(404).end(); }
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const origin = `http://127.0.0.1:${server.address().port}`;
const browser = await chromium.launch({headless: true, executablePath: process.env.CHROMIUM_PATH, args: ['--no-sandbox']});
try {
    const page = await browser.newPage();
    page.on('console', message => { if (message.type() === 'error') console.error(message.text()); });
    page.on('pageerror', error => console.error(error));
    await page.goto(origin);
    await page.evaluate(async () => {
        globalThis.__BOKHEIM_ASSET_BASE__ = '.';
        const api = await import('./web_gpui.js');
        await api.default();
        const bytes = new Uint8Array(await (await fetch('./fixture.pdf')).arrayBuffer());
        window.framesDuringPdf = 0;
        const timer = setInterval(() => window.framesDuringPdf++, 10);
        window.pdfChecks = (async () => {
            const results = [];
            for (let i = 0; i < 20; i++) results.push(await api.check_pdf(bytes));
            return results;
        })().then(result => { window.pdfResult = {result}; }, error => { window.pdfResult = {error: String(error)}; }).finally(() => clearInterval(timer));
    });
    await page.waitForFunction(() => window.pdfResult !== undefined, null, {timeout: 120000});
    const result = await page.evaluate(() => ({...window.pdfResult, frames: window.framesDuringPdf}));
    assert.equal(result.error, undefined, result.error);
    assert.equal(result.result.length, 20);
    assert.ok(result.frames > 0, 'UI event loop remains responsive while PDFium loads/renders');
    console.log(result.result[0]);
    console.log(`PASS: repeated open/close and responsive UI (${result.frames} timer ticks)`);

    const failing = await browser.newPage();
    await failing.route('**/pdfium.esm.wasm', route => route.abort());
    await failing.goto(origin);
    await failing.evaluate(() => {
        const NativeWorker = globalThis.Worker;
        globalThis.Worker = class extends NativeWorker {
            constructor(...args) {
                super(...args);
                window.pdfTestWorker = this;
            }
        };
    });
    await failing.evaluate(async () => {
        globalThis.__BOKHEIM_ASSET_BASE__ = '.';
        const api = await import('./web_gpui.js');
        await api.default();
        api.check_pdf(new Uint8Array()).then(() => window.failureResult = 'unexpected success', error => window.failureResult = String(error));
    });
    await failing.waitForFunction(() => window.failureResult !== undefined);
    assert.notEqual(await failing.evaluate(() => window.failureResult), 'unexpected success');
    console.log('PASS: worker initialization failure reaches the reader');
    await failing.unroute('**/pdfium.esm.wasm');
    const recovered = await failing.evaluate(async () => {
        const api = await import('./web_gpui.js');
        const bytes = new Uint8Array(await (await fetch('./fixture.pdf')).arrayBuffer());
        return api.check_pdf(bytes);
    });
    assert.ok(recovered.startsWith('PASS:'), recovered);
    console.log('PASS: PDF loading recovers after a failed download without reloading the page');
    const terminal = await failing.evaluate(async () => {
        const previousWorker = window.pdfTestWorker;
        previousWorker.onmessageerror();
        const api = await import('./web_gpui.js');
        try {
            await api.check_pdf(new Uint8Array());
            return { error: 'unexpected success' };
        } catch (error) {
            return { error: String(error), restarted: window.pdfTestWorker !== previousWorker };
        }
    });
    assert.match(terminal.error, /Invalid PDF worker response/);
    assert.equal(terminal.restarted, false);
    console.log('PASS: failures after initialization do not restart a worker with shared PDFium state');
} finally {
    await browser.close();
    await new Promise(resolve => server.close(resolve));
}

// Exercise the Rust callback and GPUI action propagation in a release bundle.
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import {once} from 'node:events';

const {firefox} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
const dist = new URL('../dist/', import.meta.url);
const server = createServer(async (request, response) => {
    response.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
    response.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
    const path = new URL(request.url, 'http://localhost').pathname;
    if (path === '/before') {
        response.setHeader('Content-Type', 'text/html');
        response.end('<a href="/">Open Bokheim</a>');
        return;
    }
    try {
        const file = new URL(`.${path === '/' ? '/index.html' : path}`, dist);
        if (!file.href.startsWith(dist.href)) throw new Error('Invalid path');
        response.setHeader('Content-Type', path.endsWith('.wasm') ? 'application/wasm'
            : /\.(m?js)$/.test(path) ? 'text/javascript' : path === '/' ? 'text/html' : 'application/octet-stream');
        response.end(await readFile(file));
    } catch { response.writeHead(404).end(); }
});
server.listen(0, '127.0.0.1');
await once(server, 'listening');
const origin = `http://127.0.0.1:${server.address().port}`;
let browser;
try {
    browser = await firefox.launch({headless: true, executablePath: process.env.FIREFOX_PATH,
        firefoxUserPrefs: {'dom.webgpu.enabled': true, 'gfx.webrender.all': true}});
    const page = await browser.newPage({viewport: {width: 1100, height: 850}});
    page.setDefaultTimeout(60000);
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.goto(`${origin}/before`);
    await page.getByRole('link').click();
    await page.waitForSelector('canvas');
    await page.waitForTimeout(3000);
    // With an empty library, the initial BrowserRoot owns focus. Walk the
    // sidebar to Settings; these transitions must not create route history.
    for (let i = 0; i < 6; i++) {
        await page.keyboard.press('Control+ArrowDown');
        await page.waitForTimeout(150);
    }
    const length = await page.evaluate(() => history.length);
    await page.evaluate(() => history.back());
    await page.waitForFunction(() => history.state?.__bokheimBack?.kind === 'guard');
    await page.waitForTimeout(500);
    assert.equal(page.url(), `${origin}/`, 'Settings Back must stay in GPUI');
    assert.equal(await page.evaluate(() => history.length), length);
    // At the library root the same action propagates to the global fallback.
    await page.evaluate(() => history.back());
    await page.waitForURL(`${origin}/before`);
    assert.deepEqual(errors, []);
    console.log('PASS: release WASM Back reaches GPUI, leaves Settings, then exits at library root');
} finally {
    await browser?.close();
    server.close();
    server.closeAllConnections();
}

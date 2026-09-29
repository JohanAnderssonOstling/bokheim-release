import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import {once} from 'node:events';

const {chromium, firefox} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
const index = await readFile(new URL('../index.html', import.meta.url));
const compatibility = await readFile(new URL('../compatibility.mjs', import.meta.url));
const server = createServer((request, response) => {
    response.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
    response.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
    response.setHeader('Content-Type', request.url.endsWith('.mjs') || request.url.endsWith('.js') ? 'text/javascript' : 'text/html');
    response.end(request.url === '/compatibility.mjs' ? compatibility : request.url === '/pkg/web_gpui.js'
        ? 'export default async function() { window.appInitialized = true; }' : index);
});
server.listen(0, '127.0.0.1');
await once(server, 'listening');
const url = `http://127.0.0.1:${server.address().port}`;
try {
    for (const [engine, executablePath] of [[chromium, process.env.CHROMIUM_PATH], [firefox, process.env.FIREFOX_PATH]]) {
        const browser = await engine.launch({executablePath, headless: true});
        try {
            const page = await browser.newPage();
            const appRequests = [];
            page.on('request', request => {if (request.url().endsWith('/web_gpui.js')) appRequests.push(request.url());});
            await page.goto(url);
            await page.waitForFunction(() => window.appInitialized === true);
            assert.equal(await page.locator('#startup-status').count(), 0);
            assert.equal(appRequests.length, 1);
            await page.close();
            const old = await browser.newPage({userAgent: 'Mozilla/5.0 (Linux; Android 12) Chrome/147.0.0.0 Mobile Safari/537.36'});
            await old.goto(url);
            await old.getByRole('heading', {name: 'Browser not supported'}).waitFor();
            assert.equal(await old.locator('#startup-status li').innerText(), 'Bokheim requires Chrome for Android 148 or newer.');
            assert.equal(await old.evaluate(() => window.appInitialized), undefined);
            await old.close();
            console.log(`PASS: ${engine.name()} compatibility gate and version message`);
        } finally { await browser.close(); }
    }
} finally { server.close(); server.closeAllConnections(); }

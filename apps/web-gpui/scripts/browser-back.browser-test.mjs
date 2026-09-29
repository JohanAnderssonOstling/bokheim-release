import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import {once} from 'node:events';

const {chromium, firefox} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
const bridge = await readFile(new URL('../src/browser_back.mjs', import.meta.url));
const server = createServer((request, response) => {
    if (request.url === '/browser_back.mjs') {
        response.setHeader('Content-Type', 'text/javascript');
        response.end(bridge);
    } else if (request.url === '/app') {
        response.setHeader('Content-Type', 'text/html');
        response.end(`<button id="interact">Interact</button><script type="module">
            import {installBrowserBack} from '/browser_back.mjs';
            window.calls = 0;
            window.levels = 3;
            history.replaceState({...history.state, unrelated: 'preserved'}, '');
            installBrowserBack(() => { calls++; return levels-- > 0; });
            window.ready = true;
        </script>`);
    } else {
        response.setHeader('Content-Type', 'text/html');
        response.end('<a href="/app">Open app</a>');
    }
});
server.listen(0, '127.0.0.1');
await once(server, 'listening');
const origin = `http://127.0.0.1:${server.address().port}`;

try {
    for (const [engine, executablePath] of [[chromium, process.env.CHROMIUM_PATH], [firefox, process.env.FIREFOX_PATH]]) {
        const browser = await engine.launch({headless: true, executablePath});
        try {
            const page = await browser.newPage();
            page.setDefaultTimeout(15000);
            await page.goto(origin);
            await page.getByRole('link').click();
            await page.waitForFunction(() => window.ready);
            // No interaction inside the app: Back can leave immediately.
            await page.goBack();
            assert.equal(page.url(), `${origin}/`);
            await page.getByRole('link').click();
            await page.waitForFunction(() => window.ready);
            await page.locator('#interact').click();
            const length = await page.evaluate(() => history.length);
            for (let calls = 1; calls <= 3; calls++) {
                await page.evaluate(() => history.back());
                await page.waitForFunction(n => window.calls === n && history.state.__bokheimBack.kind === 'guard', calls);
                assert.equal(await page.evaluate(() => history.length), length);
                assert.equal(await page.evaluate(() => history.state.unrelated), 'preserved');
            }
            // The fourth action propagates out of GPUI and leaves the app.
            await page.evaluate(() => history.back());
            await page.waitForURL(`${origin}/`);

            // Returning to the base entry must not accidentally dismiss UI.
            await page.goForward();
            await page.waitForFunction(() => window.ready);
            const calls = await page.evaluate(() => window.calls);
            await page.evaluate(() => history.forward());
            await page.waitForFunction(() => history.state.__bokheimBack.kind === 'guard');
            assert.equal(await page.evaluate(() => window.calls), calls);

            // Reloading an armed page must reuse its existing history guard.
            const beforeReload = await page.evaluate(() => history.length);
            await page.reload();
            await page.waitForFunction(() => window.ready);
            await page.locator('#interact').click();
            assert.equal(await page.evaluate(() => history.length), beforeReload);
            await page.evaluate(() => history.back());
            await page.waitForFunction(() => window.calls === 1);
            assert.equal(await page.evaluate(() => history.state.__bokheimBack.kind), 'guard');

            // Long-press Back/history jumps can bypass the app's guard.
            await page.evaluate(() => history.go(-2));
            await page.waitForURL(`${origin}/`);
            await page.close();

            const direct = await browser.newPage();
            direct.setDefaultTimeout(15000);
            await direct.goto(`${origin}/app`);
            await direct.waitForFunction(() => window.ready);
            const directLength = await direct.evaluate(() => history.length);
            await direct.locator('#interact').click();
            await direct.evaluate(() => { window.levels = 0; history.back(); });
            if (directLength > 1) {
                // Some browsers retain the automation tab's initial blank page.
                await direct.waitForURL('about:blank');
            } else {
                await direct.waitForFunction(() => window.calls === 1);
                await direct.waitForTimeout(300);
                await direct.locator('#interact').click();
                assert.equal(await direct.evaluate(() => history.state.__bokheimBack.kind), 'guard');
            }
            await direct.close();
            console.log(`PASS: ${engine.name()} Back dismissal, exit, Forward, reload, history jumps and direct tabs`);
        } finally { await browser.close(); }
    }
} finally {
    server.close();
    server.closeAllConnections();
}

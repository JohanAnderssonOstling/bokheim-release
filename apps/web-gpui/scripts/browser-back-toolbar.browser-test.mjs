// Linux/X11 regression using Chromium's real toolbar. Run under Xvfb to avoid
// moving the user's pointer. history.back() bypasses Chromium's intervention.
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import {once} from 'node:events';
import {execFileSync} from 'node:child_process';
import {setTimeout as delay} from 'node:timers/promises';

const {chromium} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
const bridge = await readFile(new URL('../src/browser_back.mjs', import.meta.url));
const title = `Bokheim toolbar Back regression ${process.pid}`;
const server = createServer((request, response) => {
    if (request.url === '/bridge.mjs') {
        response.setHeader('Content-Type', 'text/javascript');
        response.end(bridge);
        return;
    }
    response.setHeader('Content-Type', 'text/html');
    response.end(request.url === '/app'
        ? `<title>${title}</title><button>Interact</button><script type="module">
            import {installBrowserBack} from '/bridge.mjs';
            window.calls = 0;
            installBrowserBack(() => ++window.calls <= 3);
            window.ready = true;
          </script>`
        : '<title>Previous page</title><a href="/app">Open app</a>');
});
server.listen(0, '127.0.0.1');
await once(server, 'listening');
const origin = `http://127.0.0.1:${server.address().port}`;
let browser;
try {
    browser = await chromium.launch({executablePath: process.env.CHROMIUM_PATH, headless: false,
        args: ['--ozone-platform=x11', '--disable-gpu', '--force-device-scale-factor=1']});
    const page = await browser.newPage();
    page.setDefaultTimeout(15000);
    const cdp = await page.context().newCDPSession(page);
    await page.goto(origin);
    await page.getByRole('link').click();
    await page.waitForFunction(() => window.ready);
    await page.getByRole('button').click();
    const windowId = execFileSync('xdotool', ['search', '--onlyvisible', '--name', title], {encoding: 'utf8'}).trim().split('\n').at(-1);
    execFileSync('xdotool', ['windowfocus', '--sync', windowId]);

    async function state() {
        // Playwright page.evaluate() grants user activation by default. That
        // would mark the guard unskippable again and conceal the regression.
        const {result} = await cdp.send('Runtime.evaluate', {
            expression: '({url: location.href, calls: window.calls, kind: history.state?.__bokheimBack?.kind})',
            userGesture: false, returnByValue: true,
        });
        return result.value;
    }
    for (let calls = 1; calls <= 4; calls++) {
        // Chromium's Back button, outside the web content: no page activation.
        execFileSync('xdotool', ['mousemove', '--window', windowId, '20', '60', 'click', '1']);
        let observed;
        for (let attempt = 0; attempt < 100; attempt++) {
            await delay(50);
            observed = await state();
            if (observed.url !== `${origin}/app` || (observed.calls === calls && observed.kind === 'guard')) break;
        }
        if (calls <= 3) {
            assert.equal(observed.url, `${origin}/app`, `toolbar Back ${calls} must not leave while GPUI handles it`);
            assert.equal(observed.calls, calls);
            assert.equal(observed.kind, 'guard');
        } else {
            assert.equal(observed.url, `${origin}/`, 'unhandled Back must leave');
        }
    }
    console.log(`PASS: Chromium ${browser.version()} real toolbar Back handles three dismissals, then exits`);
} finally {
    await browser?.close();
    server.close();
    server.closeAllConnections();
}

// Full release app: use the GPUI Settings control to open the HTML form and
// verify submission crosses WASM into the account client. Intercept the login
// request so this test never authenticates against a real account service.
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
const {firefox} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
const dist = new URL('../dist/', import.meta.url);
let requests = 0;
const server = createServer(async (request, response) => {
    response.setHeader('Cross-Origin-Opener-Policy','same-origin');
    response.setHeader('Cross-Origin-Embedder-Policy','require-corp');
    try {
        const path = new URL(request.url, 'http://localhost').pathname;
        if (path === '/favicon.ico') { response.writeHead(204).end(); return; }
        if (path === '/__test/login') {
            const chunks = [];
            for await (const chunk of request) chunks.push(chunk);
            const body = Buffer.concat(chunks);
            assert.ok(body.includes(Buffer.from('form-test@example.invalid')));
            assert.ok(body.includes(Buffer.from('autofilled-test-password')));
            requests++;
            response.writeHead(401).end();
            return;
        }
        const file = new URL(`.${path === '/' ? '/index.html' : path}`, dist);
        if (!file.href.startsWith(dist.href)) throw Error('Invalid path');
        response.setHeader('Content-Type', path.endsWith('.wasm') ? 'application/wasm' : /\.(m?js)$/.test(path) ? 'text/javascript' : path === '/' ? 'text/html' : 'application/octet-stream');
        let bytes = await readFile(file);
        if (['/network_worker_loader.js', '/web_backend_database_worker_loader.js', '/coordinator.js'].some(name => path.endsWith(name))) {
            // Playwright's request routing does not cover SharedWorker fetches
            // reliably. Redirect only login at the worker's transport boundary.
            const intercept = `
                const authenticationTestFetch = globalThis.fetch.bind(globalThis);
                globalThis.fetch = (input, init) => {
                    const url = new URL(typeof input === 'string' ? input : input.url, self.location.href);
                    if (url.pathname === '/api/auth/login') {
                        const request = new Request(input, init);
                        return request.arrayBuffer().then(body => authenticationTestFetch(new URL('/__test/login', self.location.href), {method:request.method, headers:request.headers, body}));
                    }
                    return authenticationTestFetch(input, init);
                };
            `;
            bytes = Buffer.concat([Buffer.from(intercept), bytes]);
        }
        response.end(bytes);
    } catch (error) { console.error(error.message); response.writeHead(404).end(); }
});
await new Promise(resolve => server.listen(0,'127.0.0.1',resolve));
const browser = await firefox.launch({headless:true, executablePath:process.env.FIREFOX_PATH, firefoxUserPrefs:{'dom.webgpu.enabled':true,'gfx.webrender.all':true}});
try {
    const context = await browser.newContext({viewport:{width:1100,height:850}});
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    page.on('console', message => {
        if (message.type() === 'error' && !message.text().includes('Wasm assets loading')) { errors.push(message.text()); console.error(message.text()); }
    });
    await page.goto(`http://127.0.0.1:${server.address().port}/`);
    await page.waitForSelector('canvas',{timeout:90000});
    await page.waitForTimeout(3000);
    // Open Settings directly; walking the destinations would also start Store
    // network work, which is unrelated to authentication.
    await page.mouse.click(40,810);
    await page.waitForTimeout(500);
    if (process.env.AUTH_SCREENSHOT) await page.screenshot({path:process.env.AUTH_SCREENSHOT.replace('.png','-settings.png')});
    // The account panel is the first section of Settings.
    for (const y of [130,178]) {
        await page.mouse.click(425,y);
        await page.waitForTimeout(200);
        if (await page.locator('dialog').count()) break;
    }
    await page.getByRole('dialog',{name:'Sign in',exact:true}).waitFor({timeout:15000});
    await page.evaluate(() => {
        document.querySelector('#bokheim-login-username').value = 'form-test@example.invalid';
        document.querySelector('#bokheim-login-password').value = 'autofilled-test-password';
    });
    await page.getByRole('button',{name:'Sign in',exact:true}).click();
    await page.waitForFunction(() => document.querySelector('#bokheim-authentication [role=alert]')?.textContent.length > 0);
    if (process.env.AUTH_SCREENSHOT) await page.screenshot({path:process.env.AUTH_SCREENSHOT});
    assert.equal(requests,1);
    await page.keyboard.press('Escape');
    await page.locator('dialog').waitFor({state:'detached'});
    assert.equal(await page.locator('[autocomplete=current-password]').count(),0);
    for (const y of [130,178]) {
        await page.mouse.click(425,y);
        await page.waitForTimeout(200);
        if (await page.locator('dialog').count()) break;
    }
    await page.getByRole('dialog',{name:'Sign in',exact:true}).waitFor();
    assert.equal(await page.getByLabel('Password',{exact:true}).inputValue(),'');
    await page.getByRole('button',{name:'Close sign in'}).click();
    assert.deepEqual(errors,[]);
    console.log('PASS: release WASM Settings, HTML authentication, autofill submission through account client, error handling, dismissal, and clean reopening');
} finally { await browser.close(); server.close(); server.closeAllConnections(); }

// Real optimized application, served under two immutable deployment identities.
// This covers running-tab ownership and bundle isolation, not historical schemas.
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import {resolve, sep} from 'node:path';
const {chromium} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
if (!process.env.BOKHEIM_TEST_DIST) throw new Error('BOKHEIM_TEST_DIST must name an optimized web release');
const dist = resolve(process.env.BOKHEIM_TEST_DIST);
const original = await readFile(resolve(dist, 'index.html'), 'utf8');
const current = original.match(/pkg\/([a-f0-9]{24})/)?.[1];
assert.ok(current, 'expected an immutable release bundle');
const next = createHash('sha256').update(current + ':deployment-test').digest('hex').slice(0, 24);
let deployed = current;
let delayed = false;
let workerFailure = 'none';
const server = createServer(async (request, response) => {
    response.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
    response.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
    response.setHeader('Cache-Control', 'no-cache');
    try {
        const path = new URL(request.url, 'http://localhost').pathname;
        if (path === '/') { response.setHeader('Content-Type', 'text/html'); response.end(original.replaceAll(current, deployed)); return; }
        // Both deployments use this compiled application; distinct URLs exercise
        // the production coordinator/database ownership boundary without copies.
        const mapped = path.replace(`/pkg/${next}/`, `/pkg/${current}/`);
        const file = resolve(dist, '.' + mapped);
        if (!file.startsWith(dist + sep)) throw new Error('invalid fixture path');
        response.setHeader('Content-Type', /\.m?js$/.test(file) ? 'text/javascript' : file.endsWith('.wasm') ? 'application/wasm' : 'application/octet-stream');
        let bytes = await readFile(file);
        if (workerFailure !== 'none' && path.endsWith('/web_backend_database_worker_loader.js')) {
            if (workerFailure === 'once') workerFailure = 'none';
            bytes = Buffer.from('throw new Error("Injected database startup failure");');
        }
        if (!delayed && path.endsWith('/web_backend_database_worker_loader.js')) {
            delayed = true;
            // Large migrations may exceed the former 15/60-second startup cutoffs.
            // Delay one real worker and require it to reach UI readiness without
            // a page reload or a replacement worker masking the failure.
            const source = bytes.toString();
            assert.ok(source.includes('runWorkerEndpoint(initialize, true)'), 'expected database initialization boundary');
            bytes = Buffer.from(source.replace('runWorkerEndpoint(initialize, true)',
                'runWorkerEndpoint(async () => { await new Promise(resolve => setTimeout(resolve, 65000)); return initialize(); }, true)'));
        }
        response.end(bytes);
    } catch { response.writeHead(404); response.end(); }
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const browser = await chromium.launch({headless: true, executablePath: process.env.CHROMIUM_PATH || '/usr/bin/chromium',
    args: ['--no-sandbox', '--disable-dev-shm-usage', '--enable-unsafe-webgpu', '--use-angle=swiftshader', '--enable-features=Vulkan,WebGPUDeveloperFeatures']});
try {
    const context = await browser.newContext();
    const first = await context.newPage();
    const errors = [];
    let firstLoads = 0;
    first.on('framenavigated', frame => { if (frame === first.mainFrame()) firstLoads++; });
    const observe = page => {
        page.on('pageerror', error => errors.push(String(error)));
        page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
    };
    observe(first);
    const origin = `http://127.0.0.1:${server.address().port}`;
    const ready = async page => {
        try { await page.waitForFunction(() => !document.getElementById('startup-status') && document.querySelector('canvas'), undefined, {timeout: 120000}); }
        catch (error) { throw new Error(`${error}\nStartup: ${await page.locator('#startup-status').textContent().catch(() => '')}\n${errors.slice(-8).join('\n')}`); }
    };
    await first.goto(origin);
    await ready(first);
    assert.equal(firstLoads, 1, 'slow startup must not cause an automatic reload');
    assert.equal(delayed, true);
    console.log('Slow startup completed without a reload.');
    await first.evaluate(() => { window.deploymentIdentity = 'old-tab-still-running'; });
    deployed = next;
    const second = await context.newPage(); observe(second);
    await second.goto(origin);
    await second.waitForFunction(() => document.getElementById('startup-status')?.textContent.includes('Close other Bokheim tabs'), undefined, {timeout: 120000});
    assert.equal(await first.evaluate(() => window.deploymentIdentity), 'old-tab-still-running');
    assert.equal(await first.evaluate(() => globalThis.__BOKHEIM_ASSET_BASE__), './pkg/' + current);
    assert.equal(await second.evaluate(() => globalThis.__BOKHEIM_ASSET_BASE__), './pkg/' + next);
    await first.close();
    await second.evaluate(() => navigator.locks.request('bokheim-database-owner-v1', () => {}));
    await second.reload();
    await ready(second);
    assert.equal(await second.evaluate(() => globalThis.__BOKHEIM_ASSET_BASE__), './pkg/' + next);
    await context.close();
    console.log('Deployment ownership and reload passed.');
    assert.ok(!errors.some(error => /startup timed out/.test(error)), 'slow startup must not restart its worker');
    for (const failure of ['once', 'always']) {
        workerFailure = failure;
        const recovery = await browser.newContext();
        const page = await recovery.newPage(); observe(page);
        let loads = 0;
        page.on('framenavigated', frame => { if (frame === page.mainFrame()) loads++; });
        await page.goto(origin);
        if (failure === 'once') {
            await ready(page);
            assert.equal(loads, 2, 'transient worker failure should reload once and recover');
        } else {
            await page.getByRole('button', {name: 'Reload', exact: true}).waitFor({timeout: 120000});
            assert.equal(loads, 2, 'repeated failure should stop after one automatic reload');
            assert.match(await page.locator('#startup-status').textContent(), /Could not update local data/);
            workerFailure = 'none';
            await page.getByRole('button', {name: 'Reload', exact: true}).click();
            await ready(page);
            assert.equal(loads, 3, 'explicit Reload should recover after the failure is resolved');
        }
        assert.equal(await page.evaluate(() => sessionStorage.getItem('bokheim-startup-retry-v1')), null);
        await recovery.close();
    }
    console.log('Release deployment test passed: slow startup, latest bundle, existing-tab continuity, ownership conflict, automatic recovery and explicit reload.');
} finally { await browser.close(); await new Promise(resolve => server.close(resolve)); }

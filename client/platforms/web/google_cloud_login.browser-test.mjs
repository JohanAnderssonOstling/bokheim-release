// Production popup/channel code under the reader's real isolation policy.
// A local Google SDK stand-in lets this run without a Google account or tokens.
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import assert from 'node:assert/strict';
const {chromium} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
const scripts = new Map(await Promise.all(['google_cloud_login.js', 'google_cloud_login.html'].map(async name => ['/' + name, await readFile(new URL(name, import.meta.url))])));
const scopes = ['openid', 'https://www.googleapis.com/auth/userinfo.email', 'https://www.googleapis.com/auth/drive'];
const grant = {access_token: 'fixture-google-token', expires_in_seconds: 3600};
const sdk = `globalThis.google = {accounts: {oauth2: {
  initTokenClient(config) { globalThis.googleConfig = config; return {requestAccessToken() {
    if (!navigator.userActivation.isActive) throw Error('Google popup requires a user gesture');
    window.open('/fake-google', '_blank', 'popup,width=400,height=500');
  }}; },
  hasGrantedAllScopes(response, ...scopes) { return scopes.every(scope => response.scope.split(' ').includes(scope)); }
}}};`;
const fakeGoogle = `<!doctype html><button id=grant>Allow</button><button id=partial>Allow account only</button>
<script>
const response = {access_token: '${grant.access_token}', expires_in: 3600, token_type: 'Bearer', scope: '${scopes.join(' ')}'};
document.querySelector('#grant').onclick = () => { opener.googleConfig.callback(response); window.close(); };
document.querySelector('#partial').onclick = () => { opener.googleConfig.callback({...response, scope: 'openid https://www.googleapis.com/auth/userinfo.email'}); window.close(); };
</script>`;
const requests = [];
const server = createServer((req, res) => {
    requests.push(req.url);
    const popup = req.url === '/google_cloud_login.html';
    res.setHeader('Cross-Origin-Opener-Policy', req.url === '/fake-google' ? 'unsafe-none' : popup ? 'same-origin-allow-popups' : 'same-origin');
    res.setHeader('Cross-Origin-Embedder-Policy', popup || req.url === '/fake-google' ? 'unsafe-none' : 'require-corp');
    res.setHeader('Cache-Control', 'no-store');
    res.setHeader('Content-Type', req.url.endsWith('.js') ? 'text/javascript' : 'text/html');
    res.end(req.url === '/fake-google' ? fakeGoogle : scripts.get(req.url) || '<!doctype html><button id=start>Connect Google Drive</button>');
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
let browser;
try {
    browser = await chromium.launch({headless: true, executablePath: process.env.CHROMIUM_PATH, args: ['--no-sandbox']});
    const context = await browser.newContext();
    await context.route('https://accounts.google.com/gsi/client', route => route.fulfill({contentType: 'text/javascript', body: sdk}));
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.goto(`http://127.0.0.1:${server.address().port}`);
    await page.evaluate(async () => {
        window.login = await import('/google_cloud_login.js');
        window.config = login.prepareGoogleLogin('web.apps.googleusercontent.com');
        document.querySelector('#start').onclick = () => {
            window.attempt = login.beginGoogleLogin(config);
            window.outcome = login.googleLoginResult(attempt).then(grant => ({grant}), error => ({error: error.message}));
        };
    });
    const begin = async () => {
        const next = context.waitForEvent('page');
        await page.click('#start');
        const popup = await next;
        await popup.waitForSelector('#connect:enabled');
        assert.equal(await popup.evaluate(() => crossOriginIsolated), false);
        assert.equal(await popup.evaluate(() => opener), null, 'login must not depend on an opener across isolation groups');
        assert.equal(await page.evaluate(() => crossOriginIsolated), true);
        return popup;
    };
    const authorize = async (popup, button = 'grant') => {
        const next = context.waitForEvent('page');
        await popup.click('#connect');
        const google = await next;
        await google.waitForSelector('#grant');
        await google.click('#' + button);
    };

    let popup = await begin();
    await authorize(popup);
    assert.deepEqual(await page.evaluate(() => outcome), {grant});
    assert.equal(await page.evaluate(() => crossOriginIsolated), true);
    console.log('PASS: Google popup completes across isolated contexts without credentials in URLs');

    popup = await begin();
    await authorize(popup, 'partial');
    await popup.waitForSelector('#connect:enabled');
    assert.match(await popup.locator('#status').textContent(), /Allow Google Drive/);
    const pending = await page.evaluate(async () => Promise.race([outcome.then(() => false), new Promise(resolve => setTimeout(() => resolve(true), 100))]));
    assert.equal(pending, true, 'partial consent must not publish a usable grant');
    await authorize(popup);
    assert.deepEqual(await page.evaluate(() => outcome), {grant});
    console.log('PASS: partial consent stays recoverable and never publishes a Drive grant');

    popup = await begin();
    await page.evaluate(() => login.cancelGoogleLogin(attempt));
    assert.match((await page.evaluate(() => outcome)).error, /closed/);
    if (!popup.isClosed()) await popup.waitForEvent('close');
    console.log('PASS: closing the dialog cancels its sign-in window');

    popup = await begin();
    await popup.click('#cancel');
    assert.match((await page.evaluate(() => outcome)).error, /closed/);
    console.log('PASS: cancelling from the sign-in window settles the app request');

    const blocked = await page.evaluate(async () => {
        const original = window.open;
        window.open = () => null;
        const blocked = login.beginGoogleLogin(config);
        window.open = original;
        try { await login.googleLoginResult(blocked); return 'unexpected success'; }
        catch (error) { return error.message; }
    });
    assert.match(blocked, /Allow popups/);
    popup = await begin();
    const closedOnReload = popup.waitForEvent('close');
    await page.reload();
    await closedOnReload;
    assert.equal(await page.evaluate(() => crossOriginIsolated), true);
    assert.ok(requests.every(url => !url.includes(grant.access_token)));
    assert.deepEqual(errors, []);
    console.log('PASS: blocked popups fail promptly; reload cancels sign-in and keeps the reader isolated');
} finally {
    if (browser) await browser.close();
    await new Promise(resolve => server.close(resolve));
}

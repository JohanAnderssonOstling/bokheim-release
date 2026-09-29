// Run with an M4B fixture path. PLAYWRIGHT_MODULE and CHROMIUM_PATH may select
// an existing local Playwright installation/browser.
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import assert from 'node:assert/strict';
const {chromium} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
const fixture = await readFile(process.argv[2]);
const source = await readFile(new URL('./web_audio.js', import.meta.url));
const direct = process.env.BOKHEIM_DIRECT_AUDIO === '1';
let directReads = 0, revocations = 0;
let failedRenewal = false;
const server = createServer((req, res) => {
    res.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
    res.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
    if (req.url.startsWith('/api/playback/')) {
        assert.equal(req.headers.authorization, undefined);
        if (req.method === 'DELETE') { revocations++; res.writeHead(204).end(); return; }
        directReads++;
        if (req.url.includes('renewed') && !failedRenewal) {
            failedRenewal = true;
            res.writeHead(503).end(); return;
        }
        const match = /^bytes=(\d+)-(\d*)$/.exec(req.headers.range || '');
        const start = match ? Number(match[1]) : 0;
        const end = match?.[2] ? Math.min(Number(match[2]), fixture.length - 1) : fixture.length - 1;
        res.setHeader('Content-Type', 'audio/mp4');
        res.setHeader('Accept-Ranges', 'bytes');
        res.setHeader('Content-Length', end - start + 1);
        if (match) res.setHeader('Content-Range', `bytes ${start}-${end}/${fixture.length}`);
        res.writeHead(match ? 206 : 200).end(fixture.subarray(start, end + 1)); return;
    }
    res.setHeader('Content-Type', req.url === '/module.js' ? 'text/javascript' : req.url === '/book.m4b' ? 'audio/mp4' : 'text/html');
    res.end(req.url === '/module.js' ? source : req.url === '/book.m4b' ? fixture : '<button id="play">Play</button>');
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const browser = await chromium.launch({headless: true, executablePath: process.env.CHROMIUM_PATH, args: ['--no-sandbox']});
try {
    const page = await browser.newPage();
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.goto(`http://127.0.0.1:${server.address().port}`);
    await page.evaluate(async direct => {
        const {BrowserAudio} = await import('/module.js');
        const data = direct ? '/api/playback/first' : URL.createObjectURL(await (await fetch('/book.m4b')).blob());
        const grant = direct ? {url: data, expires_in_seconds: 900, checksum: 'fixture'} : null;
        window.actions = {};
        window.saves = [];
        const original = navigator.mediaSession.setActionHandler.bind(navigator.mediaSession);
        navigator.mediaSession.setActionHandler = (action, handler) => {actions[action] = handler; original(action, handler);};
        window.player = new BrowserAudio(data, new Uint8Array(), {title: 'Browser audiobook test', artist: 'Bokheim'}, 40, (...args) => saves.push(args), grant,
            async () => ({...grant, url: '/api/playback/renewed'}));
        document.querySelector('#play').onclick = () => player.play();
    }, direct);
    await page.click('#play');
    await page.waitForFunction(() => player.ready && player.playing() && player.position() > 40);
    await page.evaluate(() => actions.pause());
    const before = await page.evaluate(() => player.position());
    await page.evaluate(() => actions.seekbackward({}));
    await page.waitForFunction(() => !player.audio.seeking);
    assert.ok(Math.abs(await page.evaluate(() => player.position()) - (before - 15)) < 0.2);
    assert.ok(Math.abs(await page.evaluate(() => player.confirmedPosition()) - (before - 15)) < 0.2);
    assert.ok(Math.abs(await page.evaluate(() => saves.at(-1)[0]) - (before - 15)) < 0.2);
    await page.evaluate(() => actions.seekforward({}));
    await page.waitForFunction(() => !player.audio.seeking);
    assert.ok(Math.abs(await page.evaluate(() => player.position()) - (before + 15)) < 0.2);
    await page.evaluate(() => {player.setRate(1.5); actions.play();});
    await page.waitForFunction(() => player.playing() && player.rate() === 1.5);
    assert.equal(await page.evaluate(() => navigator.mediaSession.metadata.title), 'Browser audiobook test');
    assert.ok(await page.evaluate(() => saves.length > 1));
    if (direct) {
        await page.evaluate(() => {player.pause(); player.validUntil = 0;});
        const resume = await page.evaluate(() => player.lastConfirmedPosition());
        await page.click('#play');
        await page.waitForFunction(() => player.url.endsWith('/renewed') && player.ready && player.playing() && !player.audio.seeking);
        assert.ok(Math.abs(await page.evaluate(() => player.position()) - resume) < 2);
        assert.equal(await page.evaluate(() => player.audio.crossOrigin), 'anonymous');
        assert.ok(directReads >= 2);
        assert.equal(await page.evaluate(() => player.rate()), 1.5, 'renewal must preserve selected speed');
    }
    await page.evaluate(() => player.dispose());
    if (direct) { await page.waitForTimeout(100); assert.ok(revocations >= 2); }
    assert.equal(await page.evaluate(() => actions.seekforward), null);
    if (direct) {
        // Leave a fresh live grant to test the real browser pagehide event.
        await page.evaluate(async () => {
            const {BrowserAudio} = await import('/module.js');
            const grant = {url: '/api/playback/reload', expires_in_seconds: 900, checksum: 'fixture'};
            window.player = new BrowserAudio(grant.url, new Uint8Array(), {}, 0, () => {}, grant, async () => grant);
        });
        const beforeReload = revocations;
        await page.reload();
        await page.waitForTimeout(300);
        assert.ok(revocations > beforeReload, 'reload must revoke its grant');
    }
    assert.deepEqual(errors, []);
    console.log('PASS: real M4B playback, initial position, media actions -15/+30, speed, metadata, persistence and cleanup');
} finally {
    await browser.close();
    server.close();
}

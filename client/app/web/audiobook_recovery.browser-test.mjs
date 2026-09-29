// Exercises the production service worker and tab proxy in installed Chromium.
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import assert from 'node:assert/strict';
const {chromium} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
const scripts = new Map(await Promise.all(['audiobook_source.js', 'audiobook_service_worker.js'].map(async name => [('/' + name), await readFile(new URL(name, import.meta.url))])));
scripts.set('/player.js', await readFile(new URL('../../../apps/audiobook-player/src/web_audio.js', import.meta.url)));
const fixture = process.argv[2] ? await readFile(process.argv[2]) : null;
const pauseMs = Number(process.env.AUDIO_PAUSE_TEST_MS || 800);
assert.ok(Number.isFinite(pauseMs) && pauseMs >= 800);
let httpAborts = 0;
const server = createServer((req, res) => {
    if (req.url === '/stalled-range') {
        res.writeHead(200, {'Content-Type': 'application/octet-stream'});
        res.write(new Uint8Array([1]));
        res.on('close', () => { httpAborts++; });
        return;
    }
    if (req.url === '/http-aborts') { res.end(String(httpAborts)); return; }
    if (req.url === '/fixture.m4b' && fixture) {
        res.setHeader('Content-Type', 'audio/mp4');
        res.end(fixture);
        return;
    }
    res.setHeader('Content-Type', scripts.has(req.url) ? 'text/javascript' : 'text/html');
    res.end(scripts.get(req.url) || '<title>Audio recovery test</title>');
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
let browser;
try {
    browser = await chromium.launch({headless: true, executablePath: process.env.CHROMIUM_PATH, args: ['--no-sandbox']});
    const page = await browser.newPage();
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.goto(`http://127.0.0.1:${server.address().port}`);
    await page.evaluate(async () => {
        window.audioSources = await import('/audiobook_source.js');
        await audioSources.prepareRemoteAudio();
        window.reads = [];
        window.source = audioSources.openRemoteAudio(8, async (offset, length) => {
            reads.push([offset, length]);
            if (reads.length === 1) {
                audioSources.setRemoteAudioPaused(source, true);
                throw Object.assign(Error('injected network loss'), {retryable: true});
            }
            return new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8].slice(offset, offset + length));
        });
        window.result = fetch(source.url, {headers: {Range: 'bytes=2-5'}}).then(async response => ({status: response.status, bytes: [...new Uint8Array(await response.arrayBuffer())]}));
    });
    await page.waitForFunction(() => reads.length === 1);
    // Longer than the first backoff: paused retries must remain gated.
    await page.waitForTimeout(pauseMs);
    assert.equal(await page.evaluate(() => reads.length), 1);
    await page.evaluate(() => audioSources.setRemoteAudioPaused(source, false));
    assert.deepEqual(await page.evaluate(() => result), {status: 206, bytes: [3, 4, 5, 6]});
    assert.deepEqual(await page.evaluate(() => reads), [[2, 4], [2, 4]]);
    await page.evaluate(() => audioSources.closeRemoteAudio(source));
    await page.evaluate(() => {
        window.cancelSource = audioSources.openRemoteAudio(8, async () => {
            audioSources.setRemoteAudioPaused(cancelSource, true);
            throw Object.assign(Error('offline until request cancellation'), {retryable: true});
        });
        window.cancelRequest = new AbortController();
        window.cancelResult = fetch(cancelSource.url, {signal: cancelRequest.signal, headers: {Range: 'bytes=0-3'}})
            .then(response => response.arrayBuffer()).then(() => 'unexpected success', error => error.name);
    });
    await page.waitForFunction(() => cancelSource.recovery.resume.size === 1);
    await page.evaluate(() => cancelRequest.abort());
    assert.equal(await page.evaluate(() => cancelResult), 'AbortError');
    await page.waitForFunction(() => cancelSource.ports.size === 0 && cancelSource.recovery.resume.size === 0);
    assert.deepEqual(await page.evaluate(async () => {
        cancelSource.recovery.read = async (_, length) => new Uint8Array(length).fill(9);
        audioSources.setRemoteAudioPaused(cancelSource, false);
        const response = await fetch(cancelSource.url, {headers: {Range: 'bytes=4-7'}});
        return [...new Uint8Array(await response.arrayBuffer())];
    }), [9, 9, 9, 9]);
    await page.evaluate(() => audioSources.closeRemoteAudio(cancelSource));
    console.log('PASS: cancelled browser range releases port and retry waiter; source remains usable for a later range');
    await page.evaluate(() => {
        window.httpStarted = false;
        window.httpSource = audioSources.openRemoteAudio(8, async (_, __, signal) => {
            const response = await fetch('/stalled-range', {signal});
            httpStarted = true;
            return new Uint8Array(await response.arrayBuffer());
        });
        window.httpRequest = new AbortController();
        window.httpResult = fetch(httpSource.url, {signal: httpRequest.signal})
            .then(response => response.arrayBuffer()).then(() => 'unexpected success', error => error.name);
    });
    await page.waitForFunction(() => httpStarted);
    await page.evaluate(() => httpRequest.abort());
    assert.equal(await page.evaluate(() => httpResult), 'AbortError');
    await page.waitForFunction(async () => Number(await (await fetch('/http-aborts')).text()) > 0);
    await page.evaluate(() => audioSources.closeRemoteAudio(httpSource));
    console.log('PASS: source cancellation aborts a real stalled HTTP body');
    if (fixture) {
        await page.evaluate(async () => {
            const bytes = new Uint8Array(await (await fetch('/fixture.m4b')).arrayBuffer());
            const {BrowserAudio} = await import('/player.js');
            window.mediaReads = [];
            window.mediaFailures = 0;
            window.mediaSaves = [];
            window.mediaSource = audioSources.openRemoteAudio(bytes.length, async (offset, length) => {
                mediaReads.push([offset, length]);
                if (mediaFailures < 2) {
                    mediaFailures++;
                    throw Object.assign(Error('injected media network loss'), {retryable: true});
                }
                return bytes.slice(offset, offset + length);
            });
            window.player = new BrowserAudio(mediaSource.url, new Uint8Array(), {title: 'Recovery fixture'}, 40,
                (...args) => mediaSaves.push(args), paused => audioSources.setRemoteAudioPaused(mediaSource, paused));
            const button = document.createElement('button');
            button.id = 'play'; button.textContent = 'Play'; button.onclick = () => player.play();
            document.body.append(button);
        });
        await page.click('#play');
        await page.waitForFunction(() => player.ready && player.playing() && player.position() > 40.2);
        assert.equal(await page.evaluate(() => mediaFailures), 2);
        assert.equal(await page.evaluate(() => player.takeError()), null);
        assert.ok(await page.evaluate(() => mediaSaves.some(([position]) => position >= 40)));
        await page.evaluate(() => player.pause());
        const paused = await page.evaluate(() => player.position());
        await page.waitForTimeout(250);
        assert.equal(await page.evaluate(() => player.playing()), false);
        assert.ok(Math.abs(await page.evaluate(() => player.position()) - paused) < 0.05);
        assert.equal(await page.evaluate(() => mediaSource.recovery.paused), true);
        await page.evaluate(() => { player.dispose(); audioSources.closeRemoteAudio(mediaSource); });
        console.log('PASS: real streamed MP3-in-M4B starts at saved position after transient range failures and stays paused');
        if (process.env.AUDIO_DRAIN_TEST === '1') {
            await page.evaluate(async () => {
                const bytes = new Uint8Array(await (await fetch('/fixture.m4b')).arrayBuffer());
                const {BrowserAudio} = await import('/player.js');
                window.networkOffline = false;
                window.cutPosition = null;
                window.drainFailures = 0;
                window.mediaSource = audioSources.openRemoteAudio(bytes.length, async (offset, length) => {
                    await new Promise(resolve => setTimeout(resolve, 100));
                    if (networkOffline) {
                        drainFailures++;
                        throw Object.assign(Error('mid-playback network loss'), {retryable: true});
                    }
                    return bytes.slice(offset, offset + length);
                });
                window.player = new BrowserAudio(mediaSource.url, new Uint8Array(), {}, 40, () => {},
                    paused => audioSources.setRemoteAudioPaused(mediaSource, paused));
                player.setRate(4);
                player.audio.addEventListener('playing', () => {
                    if (cutPosition === null) { cutPosition = player.position(); networkOffline = true; }
                });
            });
            await page.click('#play');
            await page.waitForFunction(() => cutPosition !== null && drainFailures > 0 && player.position() > cutPosition + 2);
            await page.waitForFunction(() => player.buffering() && drainFailures > 0);
            const held = await page.evaluate(() => player.position());
            await page.waitForTimeout(750);
            assert.equal(await page.evaluate(() => player.position()), held);
            assert.equal(await page.evaluate(() => player.takeError()), null);
            await page.evaluate(() => { player.pause(); networkOffline = false; });
            await page.waitForTimeout(1200);
            assert.equal(await page.evaluate(() => player.playing()), false);
            assert.equal(await page.evaluate(() => player.position()), held);
            await page.click('#play');
            await page.waitForFunction(previous => player.playing() && !player.buffering() && player.position() > previous + 0.5, held);
            assert.equal(await page.evaluate(() => player.takeError()), null);
            const previousFailures = await page.evaluate(() => { networkOffline = true; return drainFailures; });
            await page.waitForFunction(previous => player.buffering() && drainFailures > previous, previousFailures);
            const autoHeld = await page.evaluate(() => player.position());
            await page.waitForTimeout(500);
            assert.equal(await page.evaluate(() => player.position()), autoHeld);
            await page.evaluate(() => { networkOffline = false; });
            // No Play action: existing playback intent must drive recovery.
            await page.waitForFunction(previous => player.playing() && !player.buffering() && player.position() > previous + 0.5, autoHeld);
            assert.equal(await page.evaluate(() => player.takeError()), null);
            await page.evaluate(() => { player.dispose(); audioSources.closeRemoteAudio(mediaSource); });
            console.log('PASS: mid-playback loss drains buffered M4B audio, holds position, respects pause across reconnect, and recovers automatically only with active intent');
        }
    }
    assert.deepEqual(errors, []);
    console.log('PASS: production audio service worker retries identical range, gates pause, resumes, and returns exact bytes');
} finally {
    if (browser) await browser.close();
    await new Promise(resolve => server.close(resolve));
}

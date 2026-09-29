// Real optimized WASM UI + backend + browser audio, with one M4B in a folder.
// Screenshots retain evidence of the canvas surfaces addressed by the clicks.
import assert from 'node:assert/strict';
import {mkdir, stat} from 'node:fs/promises';
import {join, resolve} from 'node:path';
const {firefox, chromium} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
const input = resolve(process.argv[2] || '');
assert.ok(process.argv[2] && (await stat(input)).isDirectory(), 'provide a directory containing one M4B of at least 90 seconds');
const artifacts = process.env.BOKHEIM_TEST_ARTIFACT_DIR || new URL('../../../target/audiobook-dock-browser', import.meta.url).pathname;
await mkdir(artifacts, {recursive: true});
const chrome = process.env.BROWSER === 'chromium';
const browser = await (chrome ? chromium : firefox).launch({headless: true,
    executablePath: chrome ? process.env.CHROMIUM_PATH : process.env.FIREFOX_PATH,
    args: chrome ? ['--no-sandbox'] : [],
    firefoxUserPrefs: {'dom.webgpu.enabled': true, 'gfx.webrender.all': true},
});
let page;
let phase = 'startup';
try {
    const context = await browser.newContext({viewport: {width: 1100, height: 850}});
    await context.addInitScript(() => {
        const OriginalAudio = Audio;
        window.audioElements = [];
        window.Audio = function(...args) { const audio = new OriginalAudio(...args); audioElements.push(audio); return audio; };
        Audio.prototype = OriginalAudio.prototype;
        window.mediaActions = {};
        const setHandler = navigator.mediaSession.setActionHandler.bind(navigator.mediaSession);
        navigator.mediaSession.setActionHandler = (name, handler) => { mediaActions[name] = handler; setHandler(name, handler); };
    });
    page = await context.newPage();
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    page.on('console', message => {
        if (message.text().includes('panicked at')) errors.push(message.text());
        if (process.env.BOKHEIM_TEST_VERBOSE) console.log(message.type(), message.text());
    });
    const settledCanvas = async () => {
        await page.waitForSelector('canvas', {timeout: 90000});
        // The canvas is created before library/restore requests finish.
        await page.waitForTimeout(12000);
    };
    const screenshot = name => page.screenshot({path: join(artifacts, `${name}.png`)});
    await page.goto(process.env.BOKHEIM_TEST_URL || 'http://127.0.0.1:4173');
    await settledCanvas();
    phase = 'folder import';
    const chooser = page.waitForEvent('filechooser');
    await page.mouse.click(435, 453);
    await (await chooser).setFiles(input);
    // Import status is now a GPUI canvas notification, not searchable DOM text.
    await page.waitForTimeout(15000);
    await screenshot('imported-library');
    if (process.env.BOKHEIM_REMOTE_AUDIO_FIXTURE) {
        phase = 'remote fixture preparation';
        await page.evaluate(async () => {
            const channel = new BroadcastChannel('bokheim-remote-audio-fixture');
            try {
                const route = await new Promise((resolve, reject) => {
                    const timeout = setTimeout(() => reject(Error('Fixture worker did not reply')), 15000);
                    channel.onmessage = ({data}) => { clearTimeout(timeout); data.error ? reject(Error(data.error)) : resolve(data.route); };
                    channel.postMessage({prepare: true});
                });
                const response = await fetch('/__fixture/route', {method: 'POST', body: route});
                if (!response.ok) throw Error('Could not register fixture route');
                await fetch('/__fixture/fail', {method: 'POST'});
            } finally { channel.close(); }
        });
    }
    // Leave the persistent notification visible: dock controls must remain
    // usable without requiring the user to dismiss import status first.
    phase = 'dock opening';
    const railClip = {x: 0, y: 400, width: 64, height: 450};
    const railBeforeDock = await page.screenshot({clip: railClip});
    if (process.env.BOKHEIM_REMOTE_AUDIO_FIXTURE) await page.request.post(new URL('/__fixture/hold-grant', page.url()).href);
    await page.mouse.click(185, 180);
    if (process.env.BOKHEIM_REMOTE_AUDIO_FIXTURE) {
        await page.waitForTimeout(900);
        await screenshot('loading-footer');
        assert.deepEqual(await page.screenshot({clip: railClip}), railBeforeDock, 'loading footer must leave sidebar unchanged');
        await page.request.post(new URL('/__fixture/release-grant', page.url()).href);
    }
    await page.waitForFunction(() => audioElements.at(-1)?.readyState >= 1);
    await page.waitForTimeout(500);
    await screenshot('opened-dock');
    assert.deepEqual(await page.screenshot({clip: railClip}), railBeforeDock, 'opening the dock must not cover or shorten the sidebar');
    if (await page.evaluate(() => audioElements.at(-1).paused)) await page.mouse.click(920, 826);
    await page.waitForFunction(() => !audioElements.at(-1).paused && audioElements.at(-1).currentTime > 0.5);
    assert.equal(await page.evaluate(() => audioElements.length), 1);
    await page.mouse.click(920, 826);
    await page.waitForFunction(() => audioElements[0].paused);
    if (process.env.BOKHEIM_REMOTE_AUDIO_FIXTURE) await page.waitForTimeout(1200);
    await page.mouse.click(920, 826);
    await page.waitForFunction(() => !audioElements[0].paused);
    if (process.env.BOKHEIM_REMOTE_AUDIO_FIXTURE) {
        phase = 'remote playback and seek';
        assert.ok(!(await page.evaluate(() => audioElements[0].src)).startsWith('blob:'), 'fixture still used local audio');
        await page.evaluate(async () => {
            mediaActions.seekto({seekTime: 600});
        });
        await page.waitForFunction(() => !audioElements[0].paused && !audioElements[0].seeking && audioElements[0].currentTime > 600.5, null, {timeout: 90000});
    }
    phase = 'expand and minimize';
    const before = await page.evaluate(() => audioElements[0].currentTime);
    await page.mouse.click(400, 826);
    await page.waitForTimeout(700);
    await screenshot('expanded-player');
    await page.keyboard.press('Backspace');
    await page.waitForTimeout(700);
    await screenshot('minimized-dock');
    await page.setViewportSize({width: 550, height: 850});
    await page.waitForTimeout(700);
    await screenshot('compact-dock-above-navigation');
    await page.setViewportSize({width: 1100, height: 850});
    await page.waitForTimeout(700);
    assert.equal(await page.evaluate(() => audioElements.length), 1, 'navigation reopened audio');
    assert.ok(await page.evaluate(previous => !audioElements[0].paused && audioElements[0].currentTime > previous, before));
    phase = 'persisted pause';
    await page.evaluate(() => { mediaActions.pause(); mediaActions.seekto({seekTime: 75}); });
    await page.waitForFunction(() => !audioElements[0].seeking && Math.abs(audioElements[0].currentTime - 75) < 0.3);
    await page.waitForTimeout(1500);
    phase = 'paused restoration';
    await page.reload();
    await settledCanvas();
    assert.equal(await page.evaluate(() => audioElements.length), 0, 'restoring the dock opened media before Play');
    await screenshot('restored-paused-dock');
    await page.mouse.click(920, 826);
    await page.waitForFunction(() => audioElements.at(-1)?.readyState >= 1 && !audioElements.at(-1).paused && audioElements.at(-1).currentTime >= 74.7);
    const restored = await page.evaluate(() => audioElements.at(-1).currentTime);
    assert.ok(restored < 80, `unexpected restored position ${restored}`);
    phase = 'explicit Close';
    await page.mouse.click(1060, 826);
    await page.waitForFunction(() => audioElements.at(-1).paused && mediaActions.play === null);
    await page.waitForTimeout(1500);
    await screenshot('closed-dock');
    await page.reload();
    await settledCanvas();
    await page.mouse.click(920, 826);
    await page.waitForTimeout(1000);
    assert.equal(await page.evaluate(() => audioElements.length), 0, 'closed dock returned after reload');
    await screenshot('closed-after-reload');
    assert.deepEqual(errors, []);
    console.log('PASS: real WASM import, dock playback, expand/minimize continuity, paused restore, explicit Play and durable Close');
} catch (error) {
    console.error(`Failed during ${phase}`);
    if (page) console.error(await page.evaluate(() => (window.audioElements || []).map(audio => ({
        position: audio.currentTime, paused: audio.paused, ready: audio.readyState,
        seeking: audio.seeking, error: audio.error?.code, source: audio.src.startsWith('blob:') ? 'local' : 'remote',
    }))).catch(() => 'Browser unavailable for media diagnostics'));
    if (page) await page.screenshot({path: join(artifacts, 'failure.png')}).catch(() => {});
    throw error;
} finally { await browser.close(); }

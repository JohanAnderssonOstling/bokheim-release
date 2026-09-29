// Full app test against an already served bundle. Use an M4B of at least 90s.
import assert from 'node:assert/strict';
const {firefox, chromium} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
const chrome = process.env.BROWSER === 'chromium';
const browser = await (chrome ? chromium : firefox).launch({
    executablePath: chrome ? process.env.CHROMIUM_PATH : process.env.FIREFOX_PATH, headless: true,
    args: chrome ? ['--no-sandbox'] : [],
    firefoxUserPrefs: {'dom.webgpu.enabled': true, 'gfx.webrender.all': true},
});
try {
    const context = await browser.newContext({viewport: {width: 1100, height: 850}});
    await context.addInitScript(() => {
        const OriginalAudio = Audio;
        window.audioElements = [];
        window.Audio = function(...args) {
            const audio = new OriginalAudio(...args);
            audioElements.push(audio);
            return audio;
        };
        Audio.prototype = OriginalAudio.prototype;
        window.mediaActions = {};
        const setHandler = navigator.mediaSession.setActionHandler.bind(navigator.mediaSession);
        navigator.mediaSession.setActionHandler = (name, handler) => {
            mediaActions[name] = handler;
            setHandler(name, handler);
        };
    });
    const page = await context.newPage();
    page.on('console', message => {
        if (message.type() === 'error' && !message.text().includes('Wasm assets loading')) console.error(message.text());
    });
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.goto(process.env.BOKHEIM_TEST_URL || 'http://127.0.0.1:4173');
    await page.waitForSelector('canvas');
    await page.waitForTimeout(12000); // Canvas exists before the library finishes opening.
    const chooser = page.waitForEvent('filechooser');
    await page.mouse.click(515, 470); // Empty-library Add book.
    await (await chooser).setFiles(process.argv[2]);
    await page.waitForTimeout(5000);
    await page.mouse.click(185, 180); // Imported book.
    await page.waitForFunction(() => audioElements.at(-1)?.readyState >= 1);
    await page.waitForTimeout(1500); // Let GPUI render the loaded player.
    if (process.env.BOKHEIM_TEST_SCREENSHOT) await page.screenshot({path: process.env.BOKHEIM_TEST_SCREENSHOT});
    await page.evaluate(() => {mediaActions.pause(); mediaActions.seekto({seekTime: 60});});
    await page.waitForFunction(() => !audioElements.at(-1).seeking);
    await page.evaluate(() => mediaActions.seekbackward({}));
    await page.waitForFunction(() => !audioElements.at(-1).seeking);
    assert.equal(await page.evaluate(() => audioElements.at(-1).currentTime), 45);
    await page.evaluate(() => mediaActions.seekforward({}));
    await page.waitForFunction(() => !audioElements.at(-1).seeking);
    assert.equal(await page.evaluate(() => audioElements.at(-1).currentTime), 75);
    assert.ok(await page.evaluate(() => navigator.mediaSession.metadata.title.length > 0));
    await page.mouse.click(1060, 814); // Playback speed menu.
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await page.mouse.click(955, 678); // 1.5x.
    await page.waitForFunction(() => audioElements.at(-1).playbackRate === 1.5);
    await page.mouse.click(550, 806); // Play, using the actual GPUI control.
    await page.waitForFunction(() => !audioElements.at(-1).paused);
    await page.evaluate(() => {mediaActions.pause(); mediaActions.seekto({seekTime: 75});});
    await page.waitForFunction(() => !audioElements.at(-1).seeking);
    await page.waitForTimeout(1500); // Let the worker persist the event.
    await page.reload();
    await page.waitForSelector('canvas');
    await page.waitForTimeout(2000);
    await page.mouse.click(185, 180);
    await page.waitForFunction(() => audioElements.at(-1)?.readyState >= 1 && audioElements.at(-1).currentTime >= 75);
    await page.evaluate(() => mediaActions.pause());
    const restored = await page.evaluate(() => audioElements.at(-1).currentTime);
    assert.ok(restored >= 75 && restored < 80, `restored position ${restored}`);
    await page.waitForTimeout(1500); // Let GPUI render the reopened player.
    await page.mouse.click(33, 28); // Return to library and close the reader.
    await page.waitForFunction(() => audioElements.at(-1).paused && mediaActions.play === null);
    assert.deepEqual(errors, []);
    console.log(`PASS: full WASM app import, playback, -15/+30, speed, metadata, persisted resume (${restored.toFixed(2)}s), and close`);
} finally {
    await browser.close();
}

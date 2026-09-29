// Full regular-reader regression against a served production bundle.
import assert from 'node:assert/strict';
import {mkdtemp, writeFile, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {execFileSync} from 'node:child_process';
import {fixturePdf} from './pdf-fixture.mjs';
const {firefox, chromium} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
const chrome = process.env.BROWSER === 'chromium';
const browser = await (chrome ? chromium : firefox).launch({
    executablePath: chrome ? process.env.CHROMIUM_PATH : process.env.FIREFOX_PATH,
    headless: true,
    args: chrome ? ['--no-sandbox', '--enable-unsafe-webgpu'] : [],
    firefoxUserPrefs: {'dom.webgpu.enabled': true, 'gfx.webrender.all': true},
});
const fixtureDirectory = await mkdtemp(join(tmpdir(), 'bokheim-pdf-reader-'));
await writeFile(join(fixtureDirectory, 'reader.pdf'), fixturePdf());
const pixelCounts = `
import json, sys
from io import BytesIO
from PIL import Image
image = Image.open(BytesIO(sys.stdin.buffer.read())).convert('RGB')
image = image.crop((0, 60, 1100, 650))
pixels = list(image.get_flattened_data() if hasattr(image, 'get_flattened_data') else image.getdata())
print(json.dumps({
 'red': sum(r > 150 and g < 100 and b < 100 for r,g,b in pixels),
 'blue': sum(b > 150 and r < 100 and g < 120 for r,g,b in pixels),
 'highlight': sum(r > 100 and g > 80 and b < 160 and r > b*1.3 and g > b*1.2 for r,g,b in pixels)
}))
`;
try {
    const page = await browser.newPage({viewport: {width: 1100, height: 850}});
    page.setDefaultTimeout(30000);
    const errors = [];
    page.on('pageerror', error => { errors.push(error.message); console.error(error); });
    page.on('console', message => {
        if (process.env.PDF_TEST_VERBOSE) console.log(message.type(), message.text());
        if (message.type() === 'error' && !message.text().includes('Wasm assets loading') && !message.text().includes('404')) errors.push(message.text());
    });
    await page.addInitScript(() => {
        // Assert the reader's copy operation without changing the system clipboard.
        navigator.clipboard.writeText = async text => { window.pdfCopiedText = text; };
    });
    async function counts() {
        return JSON.parse(execFileSync('python3', ['-c', pixelCounts], {input: await page.screenshot(), encoding: 'utf8'}));
    }
    async function waitForColor(color, minimumPixels = 10000) {
        for (let attempt = 0; attempt < 60; attempt++) {
            if ((await counts())[color] > minimumPixels) return;
            await page.waitForTimeout(500);
        }
        throw new Error(`PDF ${color} raster did not appear; errors: ${errors.join('; ')}`);
    }
    await page.goto(process.env.BOKHEIM_TEST_URL || 'http://127.0.0.1:4173');
    await page.waitForSelector('canvas');
    await page.waitForTimeout(12000);
    const chooser = page.waitForEvent('filechooser');
    await page.mouse.click(430, 470); // Add library opens the directory chooser.
    await (await chooser).setFiles(fixtureDirectory);
    await page.getByText(/: Import complete/).waitFor({timeout:90000});
    await page.waitForTimeout(500);
    await waitForColor('red', 500); // First-page thumbnail appears in the library.
    await page.mouse.click(185, 180); // Open the imported PDF.
    await waitForColor('red');
    await page.mouse.dblclick(260, 105); // Select "searchable" in the first page.
    await page.keyboard.press('Control+c');
    await page.waitForFunction(() => window.pdfCopiedText === 'searchable');
    await page.keyboard.press('Control+f');
    await page.keyboard.type('searchable');
    await page.waitForTimeout(1000);
    assert.ok((await counts()).highlight > 1000, 'Search highlights are painted');
    await page.mouse.click(1024, 83); // Next result in the top search toolbar.
    await waitForColor('blue');
    await page.keyboard.press('Escape');
    await page.waitForTimeout(1000); // Position writer debounce and database round trip.
    await page.keyboard.press('Backspace'); // Close before testing persisted resume.
    await page.waitForTimeout(1000);
    await page.reload();
    await page.waitForSelector('canvas');
    await waitForColor('red', 500); // Library thumbnail is ready after reload.
    await page.mouse.click(185, 180);
    await waitForColor('blue');
    await page.keyboard.press('ArrowLeft');
    await waitForColor('red');
    if (process.env.PDF_TEST_SCREENSHOT) await page.screenshot({path: process.env.PDF_TEST_SCREENSHOT});
    await page.keyboard.press('Backspace');
    await page.waitForTimeout(1000);
    assert.deepEqual(errors, []);
    console.log('PASS: regular WASM PDF reader import, colors, selection/copy, search, navigation, persisted resume, and close');
} catch (error) {
    if (process.env.PDF_TEST_FAILURE_SCREENSHOT) {
        await browser.contexts()[0]?.pages()[0]?.screenshot({path: process.env.PDF_TEST_FAILURE_SCREENSHOT});
    }
    throw error;
} finally {
    await browser.close();
    await rm(fixtureDirectory, {recursive: true, force: true});
}

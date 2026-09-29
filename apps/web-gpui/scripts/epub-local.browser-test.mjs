// Regression for local book capabilities opening without a remote prefix.
// Exercise import and the actual blocking EPUB parser in a release WASM app.
import assert from 'node:assert/strict';
import {mkdtemp, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {execFileSync} from 'node:child_process';
const {firefox} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
const directory = await mkdtemp(join(tmpdir(), 'bokheim-local-epub-'));
execFileSync('python3', ['-c', `
import sys, zipfile
from pathlib import Path
with zipfile.ZipFile(Path(sys.argv[1])/'local-range.epub', 'w') as book:
    book.writestr('mimetype', 'application/epub+zip')
    book.writestr('META-INF/container.xml', '''<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>''')
    book.writestr('content.opf', '''<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="id">urn:uuid:55eb002e-0830-43d7-8536-542f57714277</dc:identifier><dc:title>Local range regression</dc:title><dc:language>en</dc:language><meta property="dcterms:modified">2026-01-01T00:00:00Z</meta></metadata><manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/></manifest><spine><itemref idref="chapter"/></spine></package>''')
    book.writestr('nav.xhtml', '''<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><head><title>Contents</title></head><body><nav epub:type="toc"><ol><li><a href="chapter.xhtml">Local chapter</a></li></ol></nav></body></html>''')
    book.writestr('chapter.xhtml', '''<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Local chapter</title></head><body><h1>Local EPUB opened</h1><div style="background-color:#00ff00;width:400px;height:200px">Local range reader regression</div><p>This chapter was read through a local book capability without prefetched bytes.</p></body></html>''')
`, directory]);
const browser = await firefox.launch({headless:true, executablePath:process.env.FIREFOX_PATH, firefoxUserPrefs:{'dom.webgpu.enabled':true,'gfx.webrender.all':true}});
try {
    const page = await browser.newPage({viewport:{width:1100,height:850}});
    page.setDefaultTimeout(30000);
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.goto(process.env.BOKHEIM_TEST_URL || 'http://127.0.0.1:4173');
    await page.waitForSelector('canvas');
    await page.waitForTimeout(12000);
    const chooser = page.waitForEvent('filechooser');
    await page.mouse.click(430,470); // Add library opens the directory chooser.
    await (await chooser).setFiles(directory);
    await page.getByText(/: Import complete/).waitFor({timeout:90000});
    await page.waitForTimeout(500);
    await page.mouse.click(185,180);
    await page.waitForTimeout(1000);
    if (process.env.EPUB_TEST_SCREENSHOT) await page.screenshot({path:process.env.EPUB_TEST_SCREENSHOT.replace('.png', '-opening.png')});
    const greenPixels = async () => Number(execFileSync('python3', ['-c', `
import io, sys
from PIL import Image
image=Image.open(io.BytesIO(sys.stdin.buffer.read())).convert('RGB')
pixels=image.get_flattened_data() if hasattr(image, 'get_flattened_data') else image.getdata()
print(sum(1 for r,g,b in pixels if g>200 and r<60 and b<60))
`], {input:await page.screenshot(),encoding:'utf8'}).trim());
    let painted = false;
    for (let attempt=0; attempt<40; attempt++) {
        if (await greenPixels() > 20000) { painted=true; break; }
        await page.waitForTimeout(500);
    }
    if (process.env.EPUB_TEST_SCREENSHOT) await page.screenshot({path:process.env.EPUB_TEST_SCREENSHOT});
    assert.ok(painted, `Local EPUB chapter did not render: ${errors.join('; ')}`);
    await page.keyboard.press('Backspace');
    await page.waitForTimeout(1000);
    assert.ok(await greenPixels()<20000, 'Backspace must close the reader before reopening');
    await page.mouse.click(185,180);
    for (let attempt=0; attempt<30 && await greenPixels()<20000; attempt++) await page.waitForTimeout(500);
    assert.ok(await greenPixels()>20000, 'Local EPUB must reopen with a fresh capability');
    assert.deepEqual(errors, []);
    console.log('PASS: release WASM local EPUB import, chapter rendering without a prefix, close and reopen');
} catch (error) {
    if (process.env.EPUB_TEST_SCREENSHOT) await browser.contexts()[0]?.pages()[0]?.screenshot({path:process.env.EPUB_TEST_SCREENSHOT});
    throw error;
} finally {
    await browser.close();
    await rm(directory, {recursive:true,force:true});
}

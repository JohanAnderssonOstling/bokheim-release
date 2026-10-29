// Build the epub_opening_checks WASM example, run wasm-bindgen --target web,
// then: PLAYWRIGHT_MODULE=/path/to/playwright/index.mjs node this-file.mjs /path/to/pkg
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {readFile} from 'node:fs/promises';
import {createServer} from 'node:http';
import {extname, resolve, sep} from 'node:path';

const {chromium} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
const pkg = resolve(process.argv[2]);
const fixtures = new Map();
for (const name of ['standard', 'unusual', 'comment', 'legacy']) {
  fixtures.set(name, execFileSync('python3', ['-c', `
import io, sys, zipfile
name = sys.argv[1]
chapter = 'chapter.xml' if name == 'unusual' else 'chapter.xhtml'
output = io.BytesIO()
with zipfile.ZipFile(output, 'w') as archive:
    entries = [
      ('META-INF/container.xml', '<container><rootfiles><rootfile full-path="book.opf"/></rootfiles></container>'),
      ('book.opf', '<package><manifest><item id="chapter" href="' + chapter + '" media-type="application/xhtml+xml"/><item id="notes" href="notes.xhtml" media-type="application/xhtml+xml"/><item id="css" href="style.css" media-type="text/css"/></manifest><spine><itemref idref="chapter"/></spine></package>'),
      ('style.css', 'body { color: black; }'),
      ('font.bin', bytes([5]) * 131072),
      (chapter, '<html><body>Exact remote chapter</body></html>'),
      ('notes.xhtml', '<html><body>Auxiliary notes</body></html>'),
      ('image.bin', bytes([7]) * 262144)]
    for path, data in entries: archive.writestr(path, data)
    if name == 'comment': archive.comment = b'x' * 65535
sys.stdout.buffer.write(output.getvalue())
`, name], {maxBuffer:4 * 1024 * 1024}));
}
for (const name of ['cbz', 'cbz-deleted', 'cbz-legacy']) {
  fixtures.set(name, execFileSync('python3', ['-c', `
import io, sys, zipfile
output = io.BytesIO()
info = '<ComicInfo><Title>Remote comic</Title>'
if sys.argv[1] == 'cbz-deleted': info += "<Pages><Page Image='0' Type='Delete'/></Pages>"
info += '</ComicInfo>'
with zipfile.ZipFile(output, 'w') as archive:
    for name, data in [('ComicInfo.xml', info), ('page10.jpg', bytes([10]) * 524288), ('page1.jpg', bytes([1]) * 524288), ('page2.jpg', bytes([2]) * 524288), ('padding.bin', bytes([9]) * 131072)]:
        archive.writestr(name, data)
sys.stdout.buffer.write(output.getvalue())
`, name], {maxBuffer:4 * 1024 * 1024}));
}
const calls = [];
const server = createServer(async (request, response) => {
  try {
    response.setHeader('Access-Control-Allow-Origin', '*');
    response.setHeader('Access-Control-Allow-Methods', 'GET, OPTIONS');
    response.setHeader('Access-Control-Allow-Headers', 'authorization, if-range, range');
    response.setHeader('Access-Control-Expose-Headers', 'content-range, content-length, content-type, etag');
    const path = new URL(request.url, 'http://localhost').pathname;
    if (request.method === 'OPTIONS') { response.writeHead(204); return response.end(); }
    if (path.startsWith('/book/')) {
      const name = path.split('/').at(-1);
      const bytes = fixtures.get(name);
      assert.ok(bytes);
      assert.equal(request.headers.authorization, 'Bearer fixture');
      const range = request.headers.range;
      calls.push({name, range});
      if (name.endsWith('legacy') && range.includes(',')) { response.writeHead(416); return response.end(); }
      response.setHeader('ETag', `"${'d'.repeat(64)}"`);
      const ranges = range.slice('bytes='.length).split(',').map(part => {
        const [start, end] = part.split('-');
        return start === '' ? [Math.max(0, bytes.length - Number(end)), bytes.length - 1] : [Number(start), Number(end)];
      });
      const parts = ranges.map(([start, end]) => {
        assert.ok(start >= 0 && end >= start && end < bytes.length);
        return bytes.subarray(start, end + 1);
      });
      let body;
      if (ranges.length === 1) {
        response.setHeader('Content-Range', `bytes ${ranges[0][0]}-${ranges[0][1]}/${bytes.length}`);
        body = parts[0];
      } else {
        const boundary = 'browser-epub-fixture';
        response.setHeader('Content-Type', `multipart/byteranges; boundary=${boundary}`);
        body = Buffer.concat(parts.flatMap((part, i) => [Buffer.from(`--${boundary}\r\nContent-Range: bytes ${ranges[i][0]}-${ranges[i][1]}/${bytes.length}\r\n\r\n`), part, Buffer.from('\r\n')]).concat(Buffer.from(`--${boundary}--\r\n`)));
      }
      response.setHeader('Content-Length', body.length);
      response.writeHead(206);
      return response.end(body);
    }
    if (path === '/worker.js') {
      response.setHeader('Content-Type', 'text/javascript');
      return response.end(`import init, {checkEpubOpening, checkCbzOpening} from '/pkg/epub_opening_checks.js';
        onmessage = async ({data}) => {
          try { await init(); postMessage({result:JSON.parse(await (data.includes('/cbz') ? checkCbzOpening(data, data.endsWith('cbz-deleted')) : checkEpubOpening(data)))}); }
          catch (error) { postMessage({error:String(error)}); }
        };`);
    }
    if (path.startsWith('/pkg/')) {
      const file = resolve(pkg, path.slice('/pkg/'.length));
      assert.ok(file.startsWith(pkg + sep));
      response.setHeader('Content-Type', extname(file) === '.wasm' ? 'application/wasm' : 'text/javascript');
      return response.end(await readFile(file));
    }
    response.end('<!doctype html><title>EPUB opening checks</title>');
  } catch (error) {
    response.writeHead(500);
    response.end(String(error));
  }
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const browser = await chromium.launch({executablePath:process.env.CHROMIUM, headless:true, args:['--no-sandbox']});
try {
  const port = server.address().port;
  const page = await browser.newPage();
  // Different host spelling exercises cross-origin range headers and preflight.
  await page.goto(`http://localhost:${port}`);
  for (const [name, expected] of [['standard', 2], ['unusual', 3], ['comment', 4], ['legacy', 4], ['cbz', 2], ['cbz-deleted', 3], ['cbz-legacy', 6]]) {
    const result = await page.evaluate(url => new Promise((resolve, reject) => {
      const worker = new Worker('/worker.js', {type:'module'});
      const timer = setTimeout(() => { worker.terminate(); reject(Error('EPUB opening timed out')); }, 30000);
      worker.onmessage = ({data}) => { clearTimeout(timer); worker.terminate(); data.error ? reject(Error(data.error)) : resolve(data.result); };
      worker.onerror = event => { clearTimeout(timer); worker.terminate(); reject(Error(event.message)); };
      worker.postMessage(url);
    }), `http://127.0.0.1:${port}/book/${name}`);
    if (name.startsWith('cbz')) assert.equal(result.cached_reread, true);
    else assert.equal(result.eager_text, true);
    const requests = calls.filter(call => call.name === name);
    assert.equal(requests.length, expected, JSON.stringify(requests));
    assert.equal(requests[0].range, 'bytes=-65557');
    console.log(JSON.stringify({name, requests:requests.length, ...result}));
  }
} finally {
  await browser.close();
  await new Promise(resolve => server.close(resolve));
}

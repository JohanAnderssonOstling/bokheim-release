// Run against wasm-bindgen --target web output for the cloud_checks example:
// cargo build -p library-backend --target wasm32-unknown-unknown --features browser-cloud-checks --example cloud_checks
// wasm-bindgen target/wasm32-unknown-unknown/debug/examples/cloud_checks.wasm --target web --out-dir target/cloud-browser-checks
// PLAYWRIGHT_MODULE=/path/to/playwright/index.mjs node browser_cloud.browser-test.mjs /path/to/pkg
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {createServer} from 'node:http';
import {resolve, extname, sep} from 'node:path';
import {fileURLToPath} from 'node:url';

const {chromium} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
const pkg = resolve(process.argv[2] || 'target/cloud-browser-checks');
const epub = await readFile(fileURLToPath(new URL('fixtures/cloud-cover.epub', import.meta.url)));
const calls = [];
const unexpected = [];
const metadata = {id:'book', name:'Book.epub', mimeType:'application/epub+zip', parents:['folder'], size:String(epub.length), version:'1'};
const files = new Map([['book', metadata]]);
const uploads = new Map();
const content = new Map([['book', epub]]);
let nextId = 0;
let readOnly = false;
let bookQuota = 2;
let lostBookReply = true;
const worker = `
import init, {runCloudChecks, executeCpu, prepareCloudChecks} from '/pkg/cloud_checks.js';
const metadataTimers = new Set();
const setTimer = globalThis.setTimeout.bind(globalThis);
const clearTimer = globalThis.clearTimeout.bind(globalThis);
globalThis.setTimeout = (callback, milliseconds, ...args) => {
  if (milliseconds !== 120000) return setTimer(callback, milliseconds, ...args);
  const id = setTimer((...values) => { metadataTimers.delete(id); callback(...values); }, milliseconds, ...args);
  metadataTimers.add(id);
  return id;
};
globalThis.clearTimeout = id => { metadataTimers.delete(id); clearTimer(id); };
globalThis.__activeMetadataTimers = () => metadataTimers.size;
addEventListener('error', event => {
  postMessage({error:event.message, stack:event.error?.stack});
  event.preventDefault();
});
addEventListener('unhandledrejection', event => {
  postMessage({error:String(event.reason), stack:event.reason?.stack});
  event.preventDefault();
});
const getFileHandle = FileSystemDirectoryHandle.prototype.getFileHandle;
FileSystemDirectoryHandle.prototype.getFileHandle = function (name, options) {
  if (this.name === 'book' && options?.create && globalThis.__cloudFixtureStorageFault === 'book_create') {
    throw new DOMException('Fixture storage quota exhausted', 'QuotaExceededError');
  }
  return getFileHandle.call(this, name, options);
};
const flush = FileSystemSyncAccessHandle.prototype.flush;
FileSystemSyncAccessHandle.prototype.flush = function (...args) {
  if (globalThis.__cloudFixtureStorageFault === 'flush') {
    delete globalThis.__cloudFixtureStorageFault;
    throw new DOMException('Fixture storage quota exhausted', 'QuotaExceededError');
  }
  return flush.apply(this, args);
};
try {
  await init();
  await prepareCloudChecks(location.origin, new URL(location.href).searchParams.has('cpu'));
} catch (error) {
  postMessage({error:String(error), stack:error?.stack});
  throw error;
}
onmessage = async ({data}) => {
  if (data.transport === 'cpu_request') {
    try {
      const bytes = await executeCpu(data.bytes, data.source);
      data.port.postMessage({bytes}, [bytes.buffer]);
    } catch (error) { data.port.postMessage({error:String(error)}); }
    finally { data.port.close(); }
  } else {
    try { postMessage({result:JSON.parse(await runCloudChecks(data.endpoint, data.writable))}); }
    catch (error) { postMessage({error:String(error), stack:error?.stack}); }
  }
};
postMessage({ready:true});`;
const server = createServer(async (request, response) => {
  response.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
  response.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
  response.setHeader('Cross-Origin-Resource-Policy', 'same-origin');
  try {
    const url = new URL(request.url, 'http://localhost');
    const json = value => { response.setHeader('Content-Type','application/json'); response.end(JSON.stringify(value)); };
    const body = async () => { const chunks = []; for await (const chunk of request) chunks.push(chunk); return Buffer.concat(chunks); };
    if (url.pathname.startsWith('/upload/drive/v3/')) {
      calls.push({method:request.method, path:url.pathname});
      assert.equal(request.headers.authorization, 'Bearer fixture-token');
      if (request.method === 'POST' && url.pathname === '/upload/drive/v3/files') {
        const value = JSON.parse(await body());
        const book = ['New Book.epub', 'Transferred Book.epub'].includes(value.name);
        if (!book) assert.match(value.name, /^(full|browse)-[0-9a-f]{64}\.jpg$/);
        if (readOnly) {
          assert.ok(book, 'read-only libraries cannot upload thumbnails');
          response.writeHead(403); return response.end(JSON.stringify({error:{errors:[{reason:'insufficientFilePermissions'}]}}));
        }
        if (book && bookQuota) {
          bookQuota--;
          response.writeHead(403); return response.end(JSON.stringify({error:{errors:[{reason:'storageQuotaExceeded'}]}}));
        }
        uploads.set(value.id, {...value, size:request.headers['x-upload-content-length'], mimeType:book ? 'application/epub+zip' : 'image/jpeg', version:'1'});
        response.writeHead(200, {Location:`http://${request.headers.host}/upload/drive/v3/session/${value.id}`});
        return response.end();
      }
      if (request.method === 'PUT') {
        const id = url.pathname.split('/').at(-1);
        const value = uploads.get(id);
        assert.ok(value);
        const bytes = await body();
        if (request.headers['content-range'].startsWith('bytes */')) {
          assert.equal(value.name, 'New Book.epub');
          assert.equal(bytes.length, 0);
          response.writeHead(404); return response.end();
        }
        assert.equal(bytes.length, Number(value.size));
        if (value.mimeType === 'image/jpeg') assert.equal(bytes.readUInt16BE(), 0xffd8, 'generated thumbnails must be JPEGs');
        else assert.deepEqual(bytes, epub);
        assert.equal(request.headers['content-range'], `bytes 0-${bytes.length - 1}/${bytes.length}`);
        files.set(id, value);
        content.set(id, bytes);
        if (value.name === 'New Book.epub' && lostBookReply) {
          lostBookReply = false;
          response.writeHead(503); return response.end();
        }
        return json(value);
      }
      throw Error(`Unexpected upload endpoint ${url}`);
    }
    if (url.pathname.startsWith('/drive/v3/')) {
      calls.push({method:request.method, path:url.pathname, query:url.search, range:request.headers.range});
      assert.equal(request.headers.authorization, 'Bearer fixture-token');
      if (request.method === 'POST' && url.pathname === '/drive/v3/files') {
        if (readOnly) { response.writeHead(403); return response.end(); }
        const value = JSON.parse(await body());
        assert.equal(value.mimeType, 'application/vnd.google-apps.folder');
        const parent = value.parents[0];
        if (parent === 'folder') assert.equal(value.name, '.bokheim');
        else if (files.get(parent).name === '.bokheim') assert.equal(value.name, 'thumbnails');
        else assert.match(value.name, /^[0-9a-f]{66}$/);
        const folder = {id:`folder-${++nextId}`, ...value};
        files.set(folder.id, folder);
        return json(folder);
      }
      assert.equal(request.method, 'GET');
      if (url.pathname === '/drive/v3/changes/startPageToken') return json({startPageToken:'checkpoint'});
      if (url.pathname === '/drive/v3/changes') return json({changes:[], newStartPageToken:'checkpoint'});
      if (url.pathname === '/drive/v3/files/generateIds') return json({ids:[`cover-${++nextId}`]});
      if (url.pathname === '/drive/v3/files') {
        const query = url.searchParams.get('q');
        const parent = /'([^']+)' in parents/.exec(query)?.[1];
        assert.ok(parent);
        let entries = [...files.values()].filter(file => file.parents.includes(parent));
        if (query.includes("mimeType = 'application/vnd.google-apps.folder'")) entries = entries.filter(file => file.mimeType === 'application/vnd.google-apps.folder');
        return json({files:entries});
      }
      if (url.pathname.startsWith('/drive/v3/files/')) {
        const id = url.pathname.split('/').at(-1);
        const file = files.get(id);
        if (!file) { response.writeHead(404); return response.end(); }
        if (url.searchParams.get('alt') !== 'media') return json(file);
        const bytes = content.get(id);
        assert.ok(bytes);
        const range = /^bytes=(\d+)-(\d+)$/.exec(request.headers.range);
        assert.ok(range, 'provider reads must use an explicit range');
        const start = Number(range[1]), end = Number(range[2]);
        assert.ok(start <= end && end < bytes.length);
        response.writeHead(206, {'Content-Type':file.mimeType, 'Content-Range':`bytes ${start}-${end}/${bytes.length}`});
        return response.end(bytes.subarray(start, end + 1));
      }
      throw Error(`Unexpected provider endpoint ${url}`);
    }
    if (url.pathname === '/metadata/v3/books') {
      assert.equal(request.method, 'POST');
      return json({policy:'fixture', results:[]});
    }
    if (url.pathname === '/') {
      response.setHeader('Content-Type','text/html');
      return response.end('<!doctype html><title>Cloud library runtime checks</title>');
    }
    if (url.pathname === '/taxonomy.sqlite3') {
      response.setHeader('Content-Type','application/octet-stream');
      return response.end(await readFile(fileURLToPath(new URL('../../../shared/subject-projection/data/unified-taxonomy-v2.sqlite3', import.meta.url))));
    }
    if (url.pathname === '/worker.js') {
      response.setHeader('Content-Type','text/javascript');
      return response.end(worker);
    }
    if (url.pathname === '/favicon.ico') { response.writeHead(204); return response.end(); }
    assert.ok(url.pathname.startsWith('/pkg/'), `Unexpected network request ${url}`);
    const path = resolve(pkg, decodeURIComponent(url.pathname.slice(5)));
    assert.ok(path.startsWith(pkg + sep));
    response.setHeader('Content-Type', extname(path) === '.wasm' ? 'application/wasm' : 'text/javascript');
    response.end(await readFile(path));
  } catch (error) {
    unexpected.push(String(error));
    response.writeHead(500);
    response.end(String(error));
  }
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const endpoint = `http://127.0.0.1:${server.address().port}`;
const browser = await chromium.launch({executablePath:process.env.CHROMIUM || '/usr/bin/chromium', headless:true, args:['--no-sandbox']});
try {
  for (const writable of [true, false]) {
    readOnly = !writable;
    calls.length = 0;
    files.clear(); files.set('book', metadata);
    content.clear(); content.set('book', epub);
    uploads.clear();
    bookQuota = 2; lostBookReply = true;
    const page = await browser.newPage();
    page.on('console', message => console.log(`browser ${message.type()}: ${message.text()}`));
    page.on('pageerror', error => console.error('page error:', error));
    page.on('request', request => {
      if (!request.url().startsWith(endpoint + '/')) unexpected.push(`Unexpected external request ${request.url()}`);
    });
    await page.goto(endpoint);
    const result = await page.evaluate(async ({endpoint, writable}) => {
      if (!crossOriginIsolated) throw Error('Fixture requires cross-origin isolation');
      const library = new Worker('/worker.js', {type:'module'});
      const cpu = new Worker('/worker.js?cpu', {type:'module'});
      try {
        return await new Promise((resolve, reject) => {
          const timer = setTimeout(() => reject(Error('Cloud runtime checks timed out')), 90000);
          const fail = error => { clearTimeout(timer); reject(Error(error.error?.stack || error.stack || error.message || String(error))); };
          library.onerror = cpu.onerror = fail;
          let ready = 0;
          const loaded = () => { if (++ready === 2) library.postMessage({endpoint, writable}); };
          cpu.onmessage = ({data}) => { if (data.ready) loaded(); else fail(data.stack || data.error || 'Unexpected CPU message'); };
          library.onmessage = ({data}) => {
            if (data.ready) return loaded();
            if (data.transport === 'cpu_request') {
              const transfer = [data.port, data.bytes.buffer];
              if (data.source) transfer.push(data.source.port);
              return cpu.postMessage(data, transfer);
            }
            clearTimeout(timer);
            if (data.error) reject(Error(data.error + '\n' + (data.stack || '')));
            else resolve(data.result);
          };
        });
      } finally { library.terminate(); cpu.terminate(); }
    }, {endpoint, writable});
    assert.deepEqual(result, {scanned:1, provider_thumbnails:writable, local_thumbnails:true, downloaded:true, local_reader:true, quota_preserved_download:true, checksum_rejected:true, new_generation_preserved:true, evicted_and_downloaded_again:true, open_reader_preserved:true, storage_copy_quota_retry:true, browser_upload_retained:true, browser_upload_recovered:writable, browser_copy_to_drive:writable});
    assert.deepEqual(unexpected, []);
    assert.equal(uploads.size, writable ? 4 : 0, 'one thumbnail pair, one recovered import, and one library copy reach writable storage');
    assert.equal(calls.filter(call => call.method === 'PUT' && !call.query).length, writable ? 5 : 0, 'the committed book is recovered without uploading its bytes twice');
    assert.ok(calls.filter(call => call.range).length >= 3, 'inspect and offline download must reach the provider');
    console.log(JSON.stringify({writable, result, providerRequests:calls.length}));
    await page.close();
  }
} finally {
  await browser.close();
  await new Promise(resolve => server.close(resolve));
}

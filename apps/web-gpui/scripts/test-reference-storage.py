#!/usr/bin/env python3
"""Check persistent bulk reference installation and recovery using real WASM.

Build web-backend-worker with --lib --example sync_coordinator --example
startup_probe --example cpu_worker, as described in test-backend-startup.py.
Supply the exported taxonomy with --taxonomy or BOKHEIM_WEB_TAXONOMY.
Requires Python Playwright, Chromium and a matching wasm-bindgen CLI.
"""
import argparse
from functools import partial
from http.server import ThreadingHTTPServer
from io import BytesIO
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import threading
from urllib.parse import urlsplit

from playwright.sync_api import sync_playwright

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main():
    fixture = load('startup_fixture', ROOT / 'apps/web-gpui/scripts/test-backend-startup.py')
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--target-dir', type=Path, default=ROOT / 'target/web-prestart-build')
    parser.add_argument('--output', type=Path, default=ROOT / 'target/web-reference-probe')
    parser.add_argument('--taxonomy', type=Path)
    args = parser.parse_args()
    target = args.target_dir.resolve()
    output = args.output.resolve()
    fixture.prepare(target, output, args.taxonomy)
    package = output / 'pkg/probe'
    bindgen = os.environ.get('WASM_BINDGEN') or str(ROOT / 'target/wasm-bindgen-cli/bin/wasm-bindgen')
    subprocess.run([bindgen, '--target', 'web', '--out-dir', str(package / 'cpu_worker'), '--out-name', 'cpu_worker',
                    str(target / 'wasm32-unknown-unknown/release/examples/cpu_worker.wasm')], check=True)
    (package / 'cpu_worker.js').write_text('import "./profile-storage.mjs"; export {default} from "./cpu_worker/cpu_worker.js"; export * from "./cpu_worker/cpu_worker.js";\n')
    shutil.copyfile(ROOT / 'apps/web-backend-worker/cpu_worker_loader.js', package / 'cpu_worker_loader.js')
    backend = package / 'web_backend_worker.js'
    backend.write_text('import "./profile-storage.mjs";\n' + backend.read_text())
    # Offline update feed: a packaged reference must be sufficient on its own.
    coordinator = package / 'coordinator.js'
    coordinator.write_text(coordinator.read_text().replace("url.hostname === 'catalogs.bokheim.se'", "['catalogs.bokheim.se', 'updates.bokheim.se'].includes(url.hostname)"))
    profile = (HERE / 'fixtures/profile-reference-storage.mjs').read_text()
    fault = '''
const faultWrite = FileSystemSyncAccessHandle.prototype.write;
let failed = false;
FileSystemSyncAccessHandle.prototype.write = function(value, options = {}) {
    if (!failed && handles.get(this)?.logical.startsWith(REFERENCE_PREFIX) && options.at === 4096 && value.byteLength > 65536) {
        failed = true;
        faultWrite.call(this, bytes(value).subarray(0, 65536), options);
        throw new DOMException('Intentional interrupted reference import', 'QuotaExceededError');
    }
    return faultWrite.call(this, value, options);
};
'''
    mode = {'fault': None}
    server_module = load('web_server', ROOT / 'apps/web-gpui/scripts/serve.py')

    class Handler(server_module.CrossOriginIsolatedHandler):
        def log_message(self, *_):
            pass

        def send_head(self):
            if urlsplit(self.path).path.endswith('/profile-storage.mjs'):
                payload = (profile + (fault.replace('REFERENCE_PREFIX', json.dumps(mode['fault'])) if mode['fault'] else '')).encode()
                self.send_response(200)
                self.send_header('Content-Type', 'text/javascript')
                self.send_header('Content-Length', str(len(payload)))
                self.end_headers()
                return BytesIO(payload)
            return super().send_head()

    server = ThreadingHTTPServer(('127.0.0.1', 0), partial(Handler, directory=str(output)))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    url = f'http://127.0.0.1:{server.server_port}/'
    reports = []
    try:
        with sync_playwright() as p:
            with p.chromium.launch(executable_path=os.environ.get('CHROMIUM_PATH', '/usr/bin/chromium'), headless=True, args=['--no-sandbox']) as browser:
                def open_page(context, expect_failure=False, expected_libraries=0):
                    page = context.new_page()
                    page.add_init_script(fixture.INSTRUMENT)
                    page.goto(url + '?sequential', wait_until='domcontentloaded')
                    page.wait_for_function("globalThis.__probe !== undefined || document.getElementById('startup-status')?.textContent.includes('could not start')", timeout=120000)
                    if expect_failure:
                        assert page.evaluate('globalThis.__probe === undefined')
                        text = page.locator('#startup-status').text_content()
                        assert 'Intentional interrupted reference import' in text, text
                        return page, {'failure': text}
                    assert page.evaluate('globalThis.__probe !== undefined'), page.locator('#startup-status').inner_text()
                    assert page.evaluate('__probe.libraries()') == expected_libraries
                    worker = next(w for w in page.workers if 'web_backend_database_worker_loader' in w.url)
                    storage = worker.evaluate('''() => ({totals:[...__storageProfile.totals.values()],
                        resources:performance.getEntriesByType('resource').map(r=>({name:r.name, bytes:r.decodedBodySize}))})''')
                    return page, dict(ready_ms=page.evaluate('__startupProbe.ready'), **storage)

                def close_page(context, page):
                    page.close()
                    sentinel = context.new_page()
                    sentinel.goto(url + 'startup.mjs')
                    for _ in range(400):
                        if not sentinel.evaluate("async () => (await navigator.locks.query()).held.some(l=>l.name==='bokheim-database-owner-v1')"):
                            break
                        sentinel.wait_for_timeout(50)
                    else:
                        raise AssertionError('Database ownership did not release')
                    sentinel.close()

                context = browser.new_context()
                for visit in ['fresh', 'returning']:
                    page, report = open_page(context)
                    writes = [t for t in report['totals'] if t['file'].startswith('taxonomy-') and t['operation'] == 'write']
                    reads = [t for t in report['totals'] if t['file'].startswith('taxonomy-') and t['operation'] == 'read']
                    read_calls = sum(t['calls'] for t in reads)
                    read_bytes = sum(t['bytes'] for t in reads)
                    calls = sum(t['calls'] for t in writes)
                    assert calls <= 4 if visit == 'fresh' else calls == 0, writes
                    assert any(t['bytes'] > 65536 for t in writes) == (visit == 'fresh')
                    bisac_writes = [t for t in report['totals'] if t['file'].startswith('bisac-reference-') and t['operation'] == 'write']
                    bisac_calls = sum(t['calls'] for t in bisac_writes)
                    assert bisac_calls == (3 if visit == 'fresh' else 0), bisac_writes
                    assert not any(t['file'].startswith('bisac-reference-') and '-journal' in t['file'] and t['operation'] == 'write' for t in report['totals'])
                    # Returning startup reads only the OPFS pool's filename
                    # header. SQLite itself must stay closed until first use.
                    assert read_calls == (0 if visit == 'fresh' else 1), reads
                    assert read_bytes == (0 if visit == 'fresh' else 516), reads
                    reports.append(dict(case=visit, bisac_write_calls=bisac_calls, taxonomy_write_calls=calls, taxonomy_read_calls=read_calls, taxonomy_read_bytes=read_bytes, **report))
                    print(json.dumps(dict(case=visit, ready_ms=report['ready_ms'], bisac_write_calls=bisac_calls, taxonomy_write_calls=calls, taxonomy_read_calls=read_calls, taxonomy_read_bytes=read_bytes)), flush=True)
                    if visit == 'returning':
                        identifiers = page.evaluate("async () => [await __probe.create_library('First fixture'), await __probe.create_library('Second fixture')]")
                        assert identifiers[0] != identifiers[1]
                        assert page.evaluate('__probe.libraries()') == 2
                        print('PASS: two real library owners attach and query the same immutable taxonomy file', flush=True)
                    close_page(context, page)
                page, report = open_page(context, expected_libraries=2)
                reads = [t for t in report['totals'] if t['file'].startswith('taxonomy-') and t['operation'] == 'read']
                assert sum(t['bytes'] for t in reads) < 512 * 1024, reads
                assert not any(t['file'].startswith('taxonomy-') and t['operation'] == 'write' for t in report['totals'])
                reports.append(dict(case='returning with two libraries', **report))
                print(json.dumps(dict(case='returning with two libraries', ready_ms=report['ready_ms'], taxonomy_reads=reads)), flush=True)
                close_page(context, page)
                context.close()

                for prefix in ['taxonomy-', 'bisac-reference-']:
                    context = browser.new_context()
                    mode['fault'] = prefix
                    page, report = open_page(context, expect_failure=True)
                    reports.append(dict(case=f'interrupted {prefix}', **report))
                    close_page(context, page)
                    mode['fault'] = None
                    page, report = open_page(context)
                    assert any(t['file'].startswith(prefix) and t['operation'] == 'write' and t['bytes'] > 65536 for t in report['totals'])
                    reports.append(dict(case=f'recovered {prefix}', **report))
                    print(f'PASS: partial {prefix} import is discarded and retried on the next start', flush=True)
                    close_page(context, page)
                    context.close()

                # A CPU worker with no published override loads the same bundle.
                # A deliberately absent book source then returns a typed failure;
                # successful taxonomy loading is observed before that failure.
                def pack(value):
                    if value is None:
                        return b'\xc0'
                    if isinstance(value, str):
                        encoded = value.encode()
                        assert len(encoded) < 32
                        return bytes([0xa0 | len(encoded)]) + encoded
                    assert isinstance(value, dict) and len(value) < 16
                    return bytes([0x80 | len(value)]) + b''.join(pack(k) + pack(v) for k, v in value.items())

                request = list(pack({'Book': {'task': {'Inspect': {'source_name': 'probe.epub', 'format': 'Epub', 'content_hash': None}}, 'taxonomy': None}}))
                def open_cpu(context):
                    page = context.new_page()
                    page.goto(url + 'startup.mjs')
                    response = page.evaluate('''request => new Promise(resolve => {
                        const worker = new Worker('./pkg/probe/cpu_worker_loader.js', {type:'module'});
                        globalThis.cpuProbe = worker;
                        worker.onerror = e => resolve({startupError:e.message});
                        worker.onmessage = ({data}) => {
                            if (data.ready) worker.postMessage({id:1, bytes:new Uint8Array(request)});
                            else resolve({hasBytes:data.bytes instanceof Uint8Array, error:data.error});
                        };
                    })''', request)
                    return page, response

                def cpu_storage(page):
                    worker = next(w for w in page.workers if 'cpu_worker_loader.js' in w.url)
                    storage = worker.evaluate('''() => ({totals:[...__storageProfile.totals.values()],
                        resources:performance.getEntriesByType('resource').map(r=>r.name)})''')
                    assert sum(r.endswith('/taxonomy.sqlite3') for r in storage['resources']) == 1, storage
                    writes = [t for t in storage['totals'] if t['file'].startswith('bisac-reference-') and t['operation'] == 'write']
                    assert sum(t['calls'] for t in writes) == 3, writes
                    return storage

                context = browser.new_context()
                page, response = open_cpu(context)
                assert response['hasBytes'], response
                reports.append(dict(case='CPU bundled reference', response=response, **cpu_storage(page)))
                print('PASS: CPU worker uses the shared BISAC bulk importer and loads the bundled taxonomy', flush=True)
                context.close()

                context = browser.new_context()
                mode['fault'] = 'bisac-reference-'
                page, response = open_cpu(context)
                assert 'Intentional interrupted reference import' in str(response), response
                page.evaluate('cpuProbe.terminate()')
                page.close()
                mode['fault'] = None
                page, response = open_cpu(context)
                assert response['hasBytes'], response
                reports.append(dict(case='CPU recovered BISAC', response=response, **cpu_storage(page)))
                print('PASS: CPU worker discards an interrupted BISAC import and retries', flush=True)
                context.close()
        (output / 'reference-storage-results.json').write_text(json.dumps(reports, indent=2) + '\n')
    finally:
        server.shutdown()


if __name__ == '__main__':
    main()

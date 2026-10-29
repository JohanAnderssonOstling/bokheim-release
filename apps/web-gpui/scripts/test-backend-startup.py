#!/usr/bin/env python3
"""Exercise early startup with the real backend, coordinator and AppClient.

Build fixtures first (distributed WASM std / panic=abort is sufficient here):
  RUSTC_BOOTSTRAP=1 cargo build --release -p web-backend-worker --lib \
    --example sync_coordinator --example startup_probe --target wasm32-unknown-unknown
Then run this script with --target-dir if a separate Cargo target was used.
Package the prepared reference with:
  cargo run --release -p subject-projection --example export_web_taxonomy -- target/web-reference-assets
  python3 apps/web-gpui/scripts/test-backend-startup.py --taxonomy target/web-reference-assets/taxonomy.sqlite3
Requires Python Playwright, Chromium and a matching wasm-bindgen CLI. The backend
starts with the prepared reference and checks the public taxonomy feed in the
background. No account or existing app data is used.
Public catalog refresh is blocked in the fixture to isolate startup and handover.
"""
import argparse
from functools import partial
from http.server import ThreadingHTTPServer
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import threading
import time
from urllib.parse import urlsplit

from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[3]
INSTRUMENT = r"""(() => {
    globalThis.__startupProbe = {workers: [], errors: []};
    const Original = Worker;
    globalThis.Worker = new Proxy(Original, {construct(target, args) {
        __startupProbe.workers.push({url:String(args[0]), name:args[1]?.name, ms:performance.now()});
        const worker = Reflect.construct(target, args);
        worker.addEventListener('error', event => __startupProbe.errors.push(event.message));
        return worker;
    }});
    addEventListener('error', event => __startupProbe.errors.push(event.message));
    // Failure cases stay on the page so their cleanup can be inspected.
    sessionStorage.setItem('bokheim-startup-retry-v1', '1');
})();"""


def prepare(target, output, taxonomy=None):
    package = output / 'pkg/probe'
    package.mkdir(parents=True, exist_ok=True)
    bindgen = os.environ.get('WASM_BINDGEN') or str(ROOT / 'target/wasm-bindgen-cli/bin/wasm-bindgen')
    for module, binary in [('sync_coordinator', 'examples/sync_coordinator.wasm'),
                           ('web_backend_worker', 'web_backend_worker.wasm'),
                           ('startup_probe', 'examples/startup_probe.wasm')]:
        subprocess.run([bindgen, '--target', 'web', '--out-dir', str(package / module), '--out-name', module,
                        str(target / 'wasm32-unknown-unknown/release' / binary)], check=True)
        (package / f'{module}.js').write_text(f'export {{default}} from "./{module}/{module}.js";\nexport * from "./{module}/{module}.js";\n')
    for name in ['coordinator.js', 'web_backend_worker_loader.js', 'web_backend_database_worker_loader.js', 'worker_endpoint.js']:
        shutil.copyfile(ROOT / 'apps/web-backend-worker' / name, package / name)
    coordinator = package / 'coordinator.js'
    coordinator.write_text('''// Fixture: avoid an unrelated public catalog download and projection build.
const fixtureFetch = globalThis.fetch.bind(globalThis);
globalThis.fetch = (input, options) => {
    const url = new URL(input instanceof Request ? input.url : input, self.location.href);
    if (url.hostname === 'catalogs.bokheim.se') {
        const response = new Response('Catalog refresh disabled in startup fixture', {status: 503});
        // reqwest parses Response.url even for unsuccessful HTTP statuses.
        Object.defineProperty(response, 'url', {value: url.href});
        return Promise.resolve(response);
    }
    return fixtureFetch(input, options);
};
''' + coordinator.read_text())
    shutil.copyfile(ROOT / 'client/platforms/web/worker_transport.js', package / 'worker_transport.js')
    for source in (target / 'wasm32-unknown-unknown/release/build').glob('subject-projection-*/out/bisac-reference.sqlite3'):
        digest = source.with_name('bisac-reference-digest.txt').read_text()
        shutil.copyfile(source, package / f'bisac-reference-{digest}.sqlite3')
    taxonomy = taxonomy or os.environ.get('BOKHEIM_WEB_TAXONOMY') or ROOT / 'target/web-reference-assets/taxonomy.sqlite3'
    shutil.copyfile(taxonomy, package / 'taxonomy.sqlite3')
    for name in ['startup.mjs', 'backend-startup.mjs']:
        shutil.copyfile(ROOT / 'apps/web-gpui' / name, output / name)
    # Same shell lifecycle as production, with an AppClient-only UI fixture.
    (output / 'index.html').write_text('''<!doctype html><html><body>
<main id="startup-status">Opening Bokheim…</main>
<script type="module">
import {startApplication} from './startup.mjs';
import {prestartBackend} from './backend-startup.mjs';
const sequential = new URL(location.href).searchParams.has('sequential');
const backend = sequential ? {stop(){}} : prestartBackend('./pkg/probe');
if (!sequential) globalThis.__BOKHEIM_BACKEND_STARTUP__ = backend;
await startApplication(document.getElementById('startup-status'), async () => {
    const probe = await import('./pkg/probe/startup_probe/startup_probe.js');
    await probe.default();
    await probe.connect('./pkg/probe/web_backend_worker_loader.js');
    globalThis.__probe = probe;
    __startupProbe.ready = performance.now();
    __BOKHEIM_STARTUP_READY__();
}, {onFailure: () => backend.stop()});
</script></body></html>''')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--target-dir', type=Path, default=ROOT / 'target')
    parser.add_argument('--output', type=Path, default=ROOT / 'target/web-startup-probe')
    parser.add_argument('--ui-delay-ms', type=int, default=2000)
    parser.add_argument('--taxonomy', type=Path, help='Prepared asset from export_web_taxonomy; also BOKHEIM_WEB_TAXONOMY')
    args = parser.parse_args()
    output = args.output.resolve()
    prepare(args.target_dir.resolve(), output, args.taxonomy)
    spec = importlib.util.spec_from_file_location('web_server', ROOT / 'apps/web-gpui/scripts/serve.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    mode = {'backend_failure': False, 'ui_failure': False}

    class Handler(module.CrossOriginIsolatedHandler):
        def log_message(self, *_):
            pass

        def send_head(self):
            path = urlsplit(self.path).path
            if path.endswith('/startup_probe_bg.wasm'):
                time.sleep(args.ui_delay_ms / 1000)
                if mode['ui_failure']:
                    self.send_error(503, 'Intentional UI loading failure')
                    return None
            if mode['backend_failure'] and path.endswith('/web_backend_worker_bg.wasm'):
                self.send_error(503, 'Intentional backend loading failure')
                return None
            return super().send_head()

    server = ThreadingHTTPServer(('127.0.0.1', 0), partial(Handler, directory=str(output)))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    url = f'http://127.0.0.1:{server.server_port}/'
    reports = []
    try:
        with sync_playwright() as p:
            def browser():
                return p.chromium.launch(executable_path=os.environ.get('CHROMIUM_PATH', '/usr/bin/chromium'), headless=True, args=['--no-sandbox'])

            def page(context, sequential=False):
                result = context.new_page()
                result.on('console', lambda message: print(f'BROWSER {message.type}: {message.text}', flush=True) if message.type == 'error' else None)
                result.add_init_script(INSTRUMENT)
                result.goto(url + ('?sequential' if sequential else ''), wait_until='domcontentloaded')
                return result

            def until(result, expression, arg=None, timeout=60):
                # Poll the resolved value of asynchronous predicates, rather
                # than treating the returned Promise itself as readiness.
                deadline = time.monotonic() + timeout
                while time.monotonic() < deadline:
                    value = result.evaluate(expression, arg)
                    if value:
                        return value
                    result.wait_for_timeout(50)
                raise AssertionError(f'Async condition timed out: {expression}')

            def ready(result):
                result.wait_for_function("globalThis.__probe !== undefined || document.getElementById('startup-status')?.textContent.includes('could not start')", timeout=120000)
                assert result.evaluate('globalThis.__probe !== undefined'), result.locator('#startup-status').inner_text()
                try:
                    assert result.evaluate('__probe.libraries()') == 0
                except Exception:
                    print(json.dumps({'request_failure': result.evaluate('__startupProbe'), 'locks': result.evaluate('navigator.locks.query()')}), flush=True)
                    raise
                report = result.evaluate('''() => ({...__startupProbe, timeOrigin:performance.timeOrigin,
                    ui:performance.getEntriesByType('resource').find(r=>r.name.endsWith('/startup_probe_bg.wasm')).toJSON()})''')
                worker = next((w for w in result.workers if 'web_backend_database_worker_loader.js' in w.url), None)
                if worker:
                    timing = worker.evaluate('''() => ({timeOrigin:performance.timeOrigin,
                        wasm:performance.getEntriesByType('resource').find(r=>r.name.endsWith('/web_backend_worker_bg.wasm')).toJSON()})''')
                    report['backend_wasm_end_ms'] = timing['timeOrigin'] + timing['wasm']['responseEnd'] - report['timeOrigin']
                return report

            for sequential in [False, True]:
                with browser() as b:
                    context = b.new_context()
                    result = page(context, sequential)
                    report = ready(result)
                    database = [w for w in report['workers'] if 'web_backend_database_worker_loader.js' in w['url']]
                    assert len(database) == 1, report
                    assert (database[0]['ms'] < report['ui']['responseEnd']) != sequential, report
                    assert (report['backend_wasm_end_ms'] < report['ui']['responseEnd']) != sequential, report
                    assert not report['errors'], report
                    reports.append({'case': 'sequential' if sequential else 'overlap', **report})
                    print(json.dumps({'case': reports[-1]['case'], 'ready_ms': report['ready'],
                                     'database_worker_created_ms': database[0]['ms'], 'backend_download_end_ms': report['backend_wasm_end_ms'],
                                     'ui_download_end_ms': report['ui']['responseEnd']}), flush=True)

            with browser() as b:
                context = b.new_context()
                first = page(context)
                ready(first)
                original = first.evaluate("async () => (await navigator.locks.query()).held.find(lock=>lock.name==='bokheim-database-owner-v1')")
                assert original, 'First tab must own the database before handover'
                second = page(context)
                report = ready(second)
                assert not any('web_backend_database_worker_loader.js' in w['url'] for w in report['workers']), report
                first.close()
                second.wait_for_function("__startupProbe.workers.some(w=>w.url.includes('web_backend_database_worker_loader.js'))", timeout=20000)
                until(second, "async previous => (await navigator.locks.query()).held.some(lock=>lock.name==='bokheim-database-owner-v1' && lock.clientId !== previous)", arg=original['clientId'], timeout=20)
                until(second, 'async () => {try {return await __probe.libraries() === 0;} catch {return false;}}')
                locks = second.evaluate('navigator.locks.query()')
                if sum(lock['name'] == 'bokheim-database-owner-v1' for lock in locks['held']) != 1:
                    print(json.dumps({'handover_failure': second.evaluate('__startupProbe'), 'locks': locks}), flush=True)
                assert sum(lock['name'] == 'bokheim-database-owner-v1' for lock in locks['held']) == 1, locks
                reports.append({'case': 'host handover', 'locks': locks})
                print('PASS: second tab reuses backend; closing its host restores requests with one database owner', flush=True)

            for failure in ['backend_failure', 'ui_failure']:
                mode[failure] = True
                try:
                    with browser() as b:
                        result = page(b.new_context())
                        result.wait_for_function("document.getElementById('startup-status')?.textContent.includes('could not start')", timeout=60000)
                        until(result, "async () => !(await navigator.locks.query()).held.some(lock=>lock.name.startsWith('bokheim-tab-') || lock.name==='bokheim-database-owner-v1')", timeout=20)
                        reports.append({'case': failure, 'panel': result.locator('#startup-status').inner_text()})
                        print(f'PASS: {failure} surfaces without hanging and releases storage/tab locks', flush=True)
                finally:
                    mode[failure] = False
        (output / 'results.json').write_text(json.dumps(reports, indent=2) + '\n')
        print(f'Results: {output / "results.json"}', flush=True)
    finally:
        server.shutdown()


if __name__ == '__main__':
    main()

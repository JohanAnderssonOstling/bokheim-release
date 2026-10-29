#!/usr/bin/env python3
"""Measure fresh and returning visits through the application's first frame.

Uses real Chromium and the release bundle. Local runs serve Brotli sidecars
and cache immutable bundle assets, matching the deployment cache policy.
No account or existing browser data is used. Requires Python Playwright.
"""
import argparse
from datetime import datetime, timezone
from functools import partial
from http.server import ThreadingHTTPServer
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import statistics
import tempfile
import threading

from playwright.sync_api import sync_playwright

HERE = Path(__file__).resolve().parent
INSTRUMENT = (HERE / 'fixtures/profile-startup.mjs').read_text()
BACKGROUND_STUB = '''const originalFetch = globalThis.fetch.bind(globalThis);
globalThis.fetch = (input, options) => {
    const url = new URL(input instanceof Request ? input.url : input, self.location.href);
    if (['catalogs.bokheim.se', 'updates.bokheim.se'].includes(url.hostname)) {
        const response = new Response('Background refresh disabled in benchmark', {status: 503});
        Object.defineProperty(response, 'url', {value: url.href});
        return Promise.resolve(response);
    }
    return originalFetch(input, options);
};
'''
BACKEND_TIMING = '''globalThis.__backendStartupTiming = {loader:performance.now()};
const originalPortPost = MessagePort.prototype.postMessage;
MessagePort.prototype.postMessage = function(data, ...args) {
    if (data?.transport === 'backend_ready' && __backendStartupTiming.ready === undefined)
        __backendStartupTiming.ready = performance.now();
    return originalPortPost.call(this, data, ...args);
};
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument('--directory', type=Path)
    source.add_argument('--url')
    parser.add_argument('--runs', type=int, default=5)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--block-background', action='store_true', help='Local diagnostic: suppress catalog/update fetches')
    parser.add_argument('--profile-backend', action='store_true', help='Local diagnostic: timestamp backend WASM initialization and readiness')
    parser.add_argument('--profile-ui', action='store_true', help='Enable existing Rust application and graphics startup timings')
    parser.add_argument('--hardware-concurrency', type=int, choices=[2, 4], help='Diagnostic: report 2 or 4 cores to the UI worker pool')
    args = parser.parse_args()
    if args.runs < 1 or ((args.block_background or args.profile_backend) and not args.directory):
        parser.error('Runs must be positive; local diagnostics require --directory')
    server = None
    if args.directory:
        spec = importlib.util.spec_from_file_location('web_server', HERE / 'serve.py')
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)

        class Handler(module.CrossOriginIsolatedHandler):
            def log_message(self, *_):
                pass

            def send_header(self, name, value):
                if name.lower() == 'cache-control' and re.match(r'^/pkg/[0-9a-f]{24}/', self.path):
                    value = 'public, max-age=31536000, immutable'
                super().send_header(name, value)

            def send_head(self):
                if args.profile_backend and self.path.split('?')[0].endswith('/web_backend_database_worker_loader.js'):
                    path = Path(self.translate_path(self.path))
                    source = path.read_text()
                    original = 'runWorkerEndpoint(initialize, true)'
                    assert original in source
                    source = source.replace(original, '''runWorkerEndpoint(async () => {
                        __backendStartupTiming.initializeStart = performance.now();
                        const result = await initialize();
                        __backendStartupTiming.initializeEnd = performance.now();
                        return result;
                    }, true)''')
                    body = (BACKEND_TIMING + source).encode()
                    self.send_response(200)
                    self.send_header('Content-Type', 'text/javascript')
                    self.send_header('Content-Length', str(len(body)))
                    self.end_headers()
                    return io.BytesIO(body)
                if args.block_background and self.path.split('?')[0].endswith('/coordinator.js'):
                    path = Path(self.translate_path(self.path))
                    body = (BACKGROUND_STUB + path.read_text()).encode()
                    self.send_response(200)
                    self.send_header('Content-Type', 'text/javascript')
                    self.send_header('Content-Length', str(len(body)))
                    self.end_headers()
                    return io.BytesIO(body)
                return super().send_head()

        server = ThreadingHTTPServer(('127.0.0.1', 0), partial(Handler, directory=str(args.directory.resolve())))
        threading.Thread(target=server.serve_forever, daemon=True).start()
        url = f'http://127.0.0.1:{server.server_port}/'
    else:
        url = args.url.rstrip('/') + '/'
    report = {
        'date_utc': datetime.now(timezone.utc).isoformat(),
        'bundle': str(args.directory.resolve()) if args.directory else url,
        'conditions': {
            'fresh_browser_per_pair': True,
            'browser_profile': 'new persistent Chromium profile per pair',
            'http_cache_capacity_bytes': 512 * 1024 * 1024,
            'returning_reuses_storage_and_http_cache': True,
            'initial_application_state': 'signed out, no user libraries',
            'viewport': [1280, 800],
            'network': 'localhost, unthrottled, Brotli sidecars' if args.directory else 'live HTTPS, unthrottled',
            'rendering': 'headless Chromium, SwiftShader software rendering',
            'background_refresh_blocked': args.block_background,
            'backend_timing_instrumented': args.profile_backend,
            'ui_timing_instrumented': args.profile_ui,
            'reported_ui_hardware_concurrency': args.hardware_concurrency,
            'measurement': 'navigation to first draw and two animation frames after startup overlay removal',
            'browser_launch_included': False,
        },
        'runs': [],
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)

    def save():
        args.output.write_text(json.dumps(report, indent=2) + '\n')

    try:
        with sync_playwright() as playwright:
            for run in range(args.runs):
                # An isolated persistent profile retains large HTTP responses;
                # set a common capacity so local free space cannot skew the pair.
                profile = tempfile.TemporaryDirectory(prefix='bokheim-startup-')
                try:
                    context = playwright.chromium.launch_persistent_context(
                        user_data_dir=profile.name,
                        executable_path=os.environ.get('CHROMIUM_PATH', '/usr/bin/chromium'), headless=True,
                        viewport={'width': 1280, 'height': 800},
                        args=['--no-sandbox', '--enable-unsafe-swiftshader', '--use-angle=swiftshader',
                              '--disk-cache-size=536870912'])
                except Exception:
                    profile.cleanup()
                    raise
                report['browser_version'] = context.browser.version
                context.add_init_script(INSTRUMENT)
                if args.profile_ui:
                    context.add_init_script('globalThis.__BOKHEIM_STARTUP_PROFILE__ = true;')
                if args.hardware_concurrency:
                    context.add_init_script(f'Object.defineProperty(Navigator.prototype,"hardwareConcurrency",{{get:()=>{args.hardware_concurrency}}});')
                context.add_init_script("sessionStorage.setItem('bokheim-startup-retry-v1', '1');")
                try:
                    for visit in ('fresh', 'returning'):
                        page = context.new_page()
                        console_errors = []
                        startup_logs = []
                        wasm_responses = []
                        network = context.new_cdp_session(page)
                        network.send('Network.enable')
                        network.on('Network.responseReceived', lambda event: wasm_responses.append(event['response'])
                                   if re.search(r'/web_gpui_bg\.wasm(?:\?|$)', event['response']['url']) else None)
                        page.on('console', lambda message: console_errors.append(message.text) if message.type == 'error' else None)
                        page.on('console', lambda message: startup_logs.append(message.text) if 'scope=' in message.text and 'stage=' in message.text else None)
                        try:
                            page.goto(url, wait_until='domcontentloaded', timeout=120000)
                            page.wait_for_function('globalThis.__startupBenchmark?.visibleFrame !== undefined || globalThis.__startupBenchmark?.failed !== undefined', timeout=120000)
                            result = page.evaluate('''() => ({...__startupBenchmark,
                                timeOrigin:performance.timeOrigin,
                                resources:performance.getEntriesByType('resource').map(r=>r.toJSON()),
                                assetBase:globalThis.__BOKHEIM_ASSET_BASE__,
                                hardwareConcurrency:navigator.hardwareConcurrency,
                                canvases:[...document.querySelectorAll('canvas')].map(c=>({width:c.width,height:c.height}))})''')
                            assert not result['errors'], result['errors']
                            assert 'visibleFrame' in result, result.get('failure', 'Application failed before its first frame')
                            assert result['canvases'], 'No rendered application canvas'
                            ui_wasm = [resource for resource in result['resources']
                                if re.search(r'/web_gpui_bg\.wasm(?:\?|$)', resource['name'])]
                            ui_modules = [module for module in result['wasmModules']
                                if re.search(r'/web_gpui_bg\.wasm(?:\?|$)', module['url'])]
                            assert len(ui_modules) == 1, 'Missing or repeated UI WASM initialization'
                            assert len(ui_wasm) == 1, 'Missing or repeated UI WASM request'
                            result['ui_wasm_response_sources'] = [{key: response.get(key) for key in
                                ('fromServiceWorker', 'serviceWorkerResponseSource', 'fromDiskCache')} for response in wasm_responses]
                            from_worker = any(response.get('fromServiceWorker') for response in wasm_responses)
                            result['ui_wasm_http_cache_hit'] = not from_worker and ui_wasm[0]['transferSize'] == 0
                            result['ui_wasm_decoded_cache_hit'] = any(response.get('serviceWorkerResponseSource') == 'cache-storage'
                                for response in wasm_responses)
                            if visit == 'returning':
                                assert result['ui_wasm_http_cache_hit'] or result['ui_wasm_decoded_cache_hit'], 'Returning visit fetched UI WASM again; neither browser cache was reused'
                            result.update(run=run + 1, visit=visit, console_errors=console_errors, startup_logs=startup_logs, workers=[])
                            for worker in page.workers:
                                if 'web_backend_database_worker_loader' in worker.url:
                                    result['workers'].append(worker.evaluate('''() => ({url:location.href,
                                        timeOrigin:performance.timeOrigin,
                                        startup:globalThis.__backendStartupTiming ?? null,
                                        resources:performance.getEntriesByType('resource').map(r=>r.toJSON())})'''))
                            report['runs'].append(result)
                            save()
                            print(json.dumps({key: result.get(key) for key in ('run', 'visit', 'ready', 'visibleFrame', 'wasmInstantiateEnd', 'canvasCreated', 'renderer', 'ui_wasm_http_cache_hit', 'ui_wasm_decoded_cache_hit')}), flush=True)
                            if run == 0:
                                page.screenshot(path=str(args.output.with_name(args.output.stem + '-' + visit + '.png')))
                        except Exception as error:
                            report['failure'] = dict(run=run + 1, visit=visit, error=str(error), console_errors=console_errors,
                                state=page.evaluate('({benchmark:globalThis.__startupBenchmark,panel:document.getElementById("startup-status")?.innerText})'))
                            save()
                            raise
                        finally:
                            page.close()
                        if visit == 'fresh':
                            sentinel = context.new_page()
                            sentinel.goto(url + 'startup.mjs', wait_until='domcontentloaded')
                            sentinel.wait_for_function("async () => !(await navigator.locks.query()).held.some(l=>l.name==='bokheim-database-owner-v1')", timeout=30000)
                            sentinel.close()
                finally:
                    try:
                        context.close()
                    finally:
                        profile.cleanup()
        report['summary_ms'] = {}
        for visit in ('fresh', 'returning'):
            rows = [r for r in report['runs'] if r['visit'] == visit]
            report['summary_ms'][visit] = {key: {
                'median': statistics.median(r[key] for r in rows),
                'min': min(r[key] for r in rows), 'max': max(r[key] for r in rows)}
                for key in ('ready', 'visibleFrame')}
        save()
        print(json.dumps(report['summary_ms']), flush=True)
    finally:
        if server:
            server.shutdown()


if __name__ == '__main__':
    main()

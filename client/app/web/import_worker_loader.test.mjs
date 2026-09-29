import {test} from 'node:test';
import {readFileSync} from 'node:fs';
import {dirname, resolve} from 'node:path';
import {fileURLToPath, pathToFileURL} from 'node:url';

// Exercise the Rust endpoint with real MessagePorts, including the production
// binary response decoder. Build cpu_worker with web-runtime-tests first.
const output = resolve(process.env.COORDINATOR_TEST_OUTPUT || resolve(dirname(fileURLToPath(import.meta.url)), '../../.test-tmp/coordinator-policy-wasm'), 'cpu_worker');
globalThis.postMessage = () => {};
const module = await import(pathToFileURL(resolve(output, 'cpu_worker.js')));
await module.default({module_or_path: readFileSync(resolve(output, 'cpu_worker_bg.wasm'))});
test('Rust import endpoint preserves typed replies and settles requests on transport failure', async () => {
    await module.import_worker_contract();
});

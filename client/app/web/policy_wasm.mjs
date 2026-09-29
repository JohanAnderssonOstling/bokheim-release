// The policy tests exercise the real Rust/WASM exports, not a JavaScript copy.
// Build with: node app/scripts/test-coordinator-policies.mjs
import { readFile } from 'node:fs/promises';
import {pathToFileURL} from 'node:url';
import {resolve} from 'node:path';
const moduleUrl = process.env.COORDINATOR_TEST_OUTPUT ? pathToFileURL(resolve(process.env.COORDINATOR_TEST_OUTPUT, 'sync_coordinator/sync_coordinator.js')) : new URL('../../.test-tmp/coordinator-policy-wasm/sync_coordinator/sync_coordinator.js', import.meta.url);
export const policy = await import(moduleUrl.href);
await policy.default({module_or_path:await readFile(new URL('sync_coordinator_bg.wasm',moduleUrl))});

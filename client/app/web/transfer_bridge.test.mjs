import assert from 'node:assert/strict';
import { policy } from './policy_wasm.mjs';
const service = policy.transferService(256);
let failure;
globalThis.self = {
    addEventListener(_event, listener) { failure = listener; },
    postMessage(message) { assert.equal(message.transport, 'transfer_request'); service.request(message.key, message.port); },
};
globalThis.addEventListener=self.addEventListener;
globalThis.postMessage=self.postMessage;
let release;
let started;
const running = new Promise(resolve => { started = resolve; });
const complete = new Promise(resolve => { release = resolve; });
let owners = 0;
const first = policy.transferRequest('library/hash', async () => { owners++; started(); await complete; return new Uint8Array([1,2,3]); });
await running;
const second = policy.transferRequest('library/hash', () => { owners++; throw new Error('duplicate executor'); });
release();
assert.deepEqual(await first, new Uint8Array([1,2,3]));
assert.deepEqual(await second, new Uint8Array([1,2,3]));
assert.equal(owners, 1);
await assert.rejects(policy.transferRequest('library/error', async () => { throw new Error('executor failed'); }), /executor failed/);
assert.equal(service.job_count(), 0);
let markStarted;
const hasStarted = new Promise(resolve => { markStarted = resolve; });
const stalled = policy.transferRequest('library/stall', async () => { markStarted(); return new Promise(() => {}); });
await hasStarted;
const rejected = assert.rejects(stalled, /worker stopped/);
service.fail(new Error('worker stopped'));
failure({data:{transport:'sync_failed',error:'worker stopped'}});
await rejected;
console.log('PASS real transfer ports share results, propagate failures, and stop on worker failure');

import test from "node:test";
import assert from "node:assert/strict";
import { policy } from './policy_wasm.mjs';
function SyncService(run) { return policy.syncService(run,256); }

let service;
const listeners = [];
globalThis.self = {
    addEventListener(kind, listener) { listeners.push(listener); },
    postMessage(message, transfer) {
        assert(transfer.includes(message.bytes.buffer), "request bytes must be transferred");
        const received = structuredClone(message, { transfer });
        assert.equal(message.bytes.byteLength, 0, "request ownership transfers away from storage worker");
        service.request(received.key, received.bytes, received.port);
    },
};
globalThis.addEventListener=self.addEventListener;
globalThis.postMessage=self.postMessage;

test("bridge carries owned storage command bytes and results through a complete pass", async () => {
    service = new SyncService(async (bytes, storage) => {
        const first = await storage(bytes);
        return storage(first);
    });
    const input = new Uint8Array([1]);
    const result = await policy.syncRequest("library", input, async bytes => new Uint8Array([bytes[0] + 1]));
    assert.deepEqual([...result], [3]);
});

test("storage callback failure crosses both ports without hanging sync", async () => {
    service = new SyncService((bytes, storage) => storage(bytes));
    await assert.rejects(policy.syncRequest("library", new Uint8Array([1]), async () => { throw new Error("transaction failed"); }), /transaction failed/);
});

test("coordinator failure rejects outstanding backend callers", async () => {
    service = new SyncService(() => new Promise(() => {}));
    const results = ['first', 'second'].map(key => policy.syncRequest(key, new Uint8Array([1]), async bytes => bytes));
    const rejection = Promise.all(results.map(result => assert.rejects(result, /coordinator stopped/)));
    for (const listener of listeners) listener({ data: { transport: "sync_failed", error: "coordinator stopped" } });
    service.fail(new Error("coordinator stopped"));
    await rejection;
});

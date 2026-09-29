import test from "node:test";
import assert from "node:assert/strict";
import { policy } from './policy_wasm.mjs';
function SyncService(run,limit=256) { return policy.syncService(run,limit); }

function request(service, key, value, storage = async (bytes) => bytes) {
    const { port1, port2 } = new MessageChannel();
    const result = new Promise((resolve, reject) => {
        port1.onmessage = async ({ data }) => {
            if (data.kind === "storage") {
                try { port1.postMessage({ id: data.id, bytes: await storage(data.bytes) }); }
                catch (error) { port1.postMessage({ id: data.id, error: String(error) }); }
            } else {
                port1.close();
                if (data.kind === "error") reject(new Error(data.error));
                else resolve(data.bytes[0]);
            }
        };
    });
    service.request(key, new Uint8Array([value]), port2);
    return result;
}
const tick = () => new Promise((resolve) => setTimeout(resolve, 10));

test("same library serializes passes and coalesces a burst into one follow-up", async () => {
    const calls = [];
    const service = new SyncService((bytes, storage) => new Promise((resolve) => calls.push({ value: bytes[0], resolve })));
    const first = request(service, "library", 1);
    const second = request(service, "library", 2);
    const third = request(service, "library", 3);
    assert.equal(calls.length, 1);
    calls[0].resolve(new Uint8Array([10]));
    assert.equal(await first, 10);
    assert.equal(calls.length, 2);
    assert.equal(calls[1].value, 3);
    calls[1].resolve(new Uint8Array([30]));
    assert.deepEqual(await Promise.all([second, third]), [30, 30]);
    assert.equal(service.count, 0);
});

test("a stalled pass does not block another library or asynchronous storage", async () => {
    let release;
    const service = new SyncService(async (bytes, storage) => {
        if (bytes[0] === 1) await new Promise((resolve) => { release = resolve; });
        return storage(bytes);
    });
    const stalled = request(service, "first", 1);
    assert.equal(await request(service, "second", 2, async () => new Uint8Array([20])), 20);
    release();
    assert.equal(await stalled, 1);
});

test("failed pass releases its library and storage failure reaches the caller", async () => {
    const service = new SyncService((bytes, storage) => storage(bytes));
    await assert.rejects(request(service, "library", 1, async () => { throw new Error("database unavailable"); }), /database unavailable/);
    assert.equal(await request(service, "library", 2), 2);
});

test("overload rejects excess triggers without losing accepted work", async () => {
    let release;
    const service = new SyncService(() => new Promise((resolve) => { release = resolve; }), 1);
    const first = request(service, "library", 1);
    await assert.rejects(request(service, "library", 2), /queue is full/);
    release(new Uint8Array([1]));
    assert.equal(await first, 1);
});

test("failure rejects storage in every active library and queued follow-ups", async () => {
    const service = new SyncService((bytes, storage) => storage(bytes));
    const first = request(service, "library", 1, () => new Promise(() => {}));
    const next = request(service, "library", 2);
    const other = request(service, "other", 3, () => new Promise(() => {}));
    const rejected = Promise.all([first, next, other].map(result => assert.rejects(result, /worker stopped/)));
    await tick();
    service.fail(new Error("worker stopped"));
    await rejected;
    assert.equal(service.count, 0);
});

test("shutdown rejects an active pass even when its HTTP never settles", async () => {
    const service = new SyncService(() => new Promise(() => {}));
    const active = request(service, "library", 1);
    const rejected = assert.rejects(active, /shutdown/);
    service.fail(new Error("shutdown"));
    await rejected;
    await assert.rejects(request(service, "library", 2), /shutdown/);
    assert.equal(service.count, 0);
});


test("overlapping storage commands are rejected without losing the active reply", async () => {
    const service = new SyncService(async (bytes, storage) => {
        const first = storage(bytes);
        await assert.rejects(storage(bytes), /command already active/);
        return first;
    });
    assert.equal(await request(service, "library", 7), 7);
});

test("a late storage reply cannot complete the next command", async () => {
    const service = new SyncService(async (bytes, storage) => storage(await storage(bytes)));
    const {port1, port2} = new MessageChannel();
    let previous;
    const result = new Promise((resolve, reject) => {
        port1.onmessage = ({data}) => {
            if (data.kind === 'storage') {
                if (previous !== undefined) port1.postMessage({id:previous,bytes:new Uint8Array([99])});
                previous = data.id;
                port1.postMessage({id:data.id,bytes:new Uint8Array([data.bytes[0]+1])});
            } else {
                port1.close();
                if (data.kind === 'error') reject(Error(data.error));
                else resolve(data.bytes[0]);
            }
        };
    });
    service.request('library',new Uint8Array([1]),port2);
    assert.equal(await result,3);
});

test("cancelling an inactive pass releases a queued active pass for the same library", async () => {
    const service = new SyncService(bytes => bytes[0] === 1 ? new Promise(() => {}) : Promise.resolve(bytes));
    const {port1,port2} = new MessageChannel();
    service.request("library",new Uint8Array([1]),port2);
    const next = request(service,"library",2);
    try {
        port1.postMessage({kind:"cancel"});
        port1.close();
        assert.equal(await next,2);
        assert.equal(service.count,0);
    } finally {
        port1.close();
        service.fail(new Error("cleanup"));
    }
});

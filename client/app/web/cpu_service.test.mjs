import test from "node:test";
import assert from "node:assert/strict";
import { policy } from "./policy_wasm.mjs";

function setup(limit) {
    const workers = [];
    const service = policy.cpuService(() => {
        const worker = { sent: [], terminated: false, postMessage(message) { this.sent.push(message); }, terminate() { this.terminated = true; } };
        workers.push(worker);
        return worker;
    }, limit ?? 32);
    return { service, workers };
}

test("CPU starts lazily and executes only one job at a time", async () => {
    const { service, workers } = setup();
    assert.equal(workers.length, 0);
    const first = service.request(new Uint8Array([1]));
    const second = service.request(new Uint8Array([2]));
    assert.equal(workers.length, 1);
    const worker = workers[0];
    assert.equal(worker.sent.length, 0);
    worker.onmessage({ data: { ready: true } });
    assert.equal(worker.sent.length, 1);
    worker.onmessage({ data: { id: worker.sent[0].id, bytes: new Uint8Array([3]) } });
    assert.deepEqual(await first, new Uint8Array([3]));
    assert.equal(worker.sent.length, 2);
    worker.onmessage({ data: { id: worker.sent[1].id, bytes: new Uint8Array([4]) } });
    assert.deepEqual(await second, new Uint8Array([4]));
    const later = service.request(new Uint8Array([5]));
    assert.equal(workers.length, 1, "an idle worker is reused");
    worker.onmessage({ data: { id: worker.sent[2].id, bytes: new Uint8Array([6]) } });
    assert.deepEqual(await later, new Uint8Array([6]));
});

test("worker failure rejects active and queued jobs and permits a later restart", async () => {
    const { service, workers } = setup();
    const first = service.request(new Uint8Array());
    const second = service.request(new Uint8Array());
    const failures = Promise.all([assert.rejects(first, /crashed/), assert.rejects(second, /crashed/)]);
    workers[0].onmessage({ data: { ready: true } });
    workers[0].onerror({ preventDefault() {}, message: "crashed" });
    await failures;
    assert.equal(workers[0].terminated, true);
    const retry = service.request(new Uint8Array());
    assert.equal(workers.length, 2);
    workers[1].onmessage({ data: { ready: true } });
    workers[1].onmessage({ data: { id: workers[1].sent[0].id, bytes: new Uint8Array([9]) } });
    assert.deepEqual(await retry, new Uint8Array([9]));
});

test("queue overload is rejected without disrupting accepted work", async () => {
    const { service, workers } = setup(1);
    const first = service.request(new Uint8Array());
    await assert.rejects(service.request(new Uint8Array()), /queue is full/);
    const done = assert.rejects(first, /shutdown/);
    service.fail(new Error("shutdown"));
    await done;
    assert.equal(workers[0].terminated, true);
});

test("an unexpected worker response rejects active and queued jobs", async () => {
    const {service,workers}=setup();
    const first=service.request(new Uint8Array([1]));
    const second=service.request(new Uint8Array([2]));
    const rejected=Promise.all([assert.rejects(first,/Unexpected/),assert.rejects(second,/Unexpected/)]);
    workers[0].onmessage({data:{ready:true}});
    workers[0].onmessage({data:{id:999,bytes:new Uint8Array()}});
    await rejected;
    assert.equal(workers[0].terminated,true);
});

test("worker construction failure allows a fresh startup on the next request", async () => {
    let attempts = 0;
    const worker = {sent: [], postMessage(message) {this.sent.push(message);}, terminate() {}};
    const service = policy.cpuService(() => {
        if (++attempts === 1) throw Error("construction failed");
        return worker;
    }, 1);
    await assert.rejects(service.request(new Uint8Array()), /construction failed/);
    const retry = service.request(new Uint8Array());
    assert.equal(attempts, 2);
    worker.onmessage({data: {ready: true}});
    worker.onmessage({data: {id: worker.sent[0].id, bytes: new Uint8Array([7])}});
    assert.deepEqual(await retry, new Uint8Array([7]));
});

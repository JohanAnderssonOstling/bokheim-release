import test from "node:test";
import assert from "node:assert/strict";
import {policy} from "./policy_wasm.mjs";

function receive(port) {
    return new Promise((resolve) => {
        port.addEventListener("message", ({ data }) => resolve(data), { once: true });
        port.start();
    });
}

test("coordinator isolates tab sessions and retains other tabs after disconnect", async () => {
    const sessions = [];
    const relay = policy.clientRelay((port) => sessions.push(port));
    const first = new MessageChannel();
    const second = new MessageChannel();
    relay.connect(first.port2);
    relay.connect(second.port2);
    const incoming = receive(sessions[0]);
    first.port1.postMessage(new Uint8Array([1, 2]));
    assert.deepEqual(await incoming, new Uint8Array([1, 2]));
    const response = receive(second.port1);
    sessions[1].postMessage(new Uint8Array([3, 4]));
    assert.deepEqual(await response, new Uint8Array([3, 4]));
    const disconnected = receive(sessions[0]);
    first.port1.postMessage("disconnect");
    assert.equal(await disconnected, "disconnect");
    assert.equal(relay.client_count(), 1);
    const stillConnected = receive(second.port1);
    sessions[1].postMessage(new Uint8Array([5]));
    assert.deepEqual(await stillConnected, new Uint8Array([5]));
    const failed = receive(second.port1);
    const released = receive(sessions[1]);
    relay.fail("storage stopped");
    assert.deepEqual(await failed, { kind: "failed", error: "storage stopped" });
    assert.equal(await released, "disconnect");
    for (const port of [...sessions, first.port1, second.port1]) port.close();
});

test("attachment failure rejects the caller and releases the failed client", async () => {
    const relay = policy.clientRelay(() => { throw Error("storage unavailable"); });
    const tab = new MessageChannel();
    const failed = receive(tab.port1);
    assert.throws(() => relay.connect(tab.port2), /storage unavailable/);
    assert.equal(relay.client_count(), 0);
    assert.deepEqual(await failed, {kind: "failed", error: "storage unavailable"});
    tab.port1.close();
});

test("channel construction failure releases the registered client", async () => {
    const relay = policy.clientRelay(() => assert.fail("connector must not run"));
    const tab = new MessageChannel();
    const failed = receive(tab.port1);
    const NativeMessageChannel = globalThis.MessageChannel;
    try {
        globalThis.MessageChannel = class { constructor() { throw Error("channel unavailable"); } };
        assert.throws(() => relay.connect(tab.port2), /channel unavailable/);
    } finally {
        globalThis.MessageChannel = NativeMessageChannel;
    }
    assert.equal(relay.client_count(), 0);
    assert.deepEqual(await failed, {kind: "failed", error: "channel unavailable"});
    tab.port1.close();
});

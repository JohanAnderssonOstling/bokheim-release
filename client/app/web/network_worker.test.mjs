import test from "node:test";
import {once} from "node:events";
import assert from "node:assert/strict";
import {policy} from "./policy_wasm.mjs";
const serveRequest=(port,input,fetch)=>policy.serveNetwork(port,input,fetch);
async function waitForAbort(signal) {
    if (!signal.aborted) await once(signal, "abort", {signal: AbortSignal.timeout(5000)});
}

let execute;
globalThis.self = globalThis;
globalThis.addEventListener = () => {};
globalThis.postMessage = message => serveRequest(message.port, message.request, execute);
policy.installFetchBridge();

test("network bridge preserves request bytes and status and streams responses on demand", async () => {
    let produced = 0;
    execute = async (url, options) => {
        assert.equal(url, "https://example.test/sync");
        assert.equal(options.method, "POST");
        assert.equal(new Headers(options.headers).get("authorization"), "Bearer test");
        assert.deepEqual(options.body, new Uint8Array([1, 2]));
        return new Response(new ReadableStream({
            pull(controller) {
                produced++;
                if (produced <= 2) controller.enqueue(new Uint8Array([produced]));
                else controller.close();
            },
        }, { highWaterMark: 0 }), { status: 201, headers: { "content-type": "application/octet-stream" } });
    };
    const response = await fetch("https://example.test/sync", { method: "POST", headers: { authorization: "Bearer test" }, body: new Uint8Array([1, 2]) });
    assert.equal(response.status, 201);
    assert.equal(produced, 0, "headers must not eagerly drain a book body");
    assert.equal(response.headers.get("content-type"), "application/octet-stream");
    const reader = response.body.getReader();
    assert.deepEqual((await reader.read()).value, new Uint8Array([1]));
    assert.equal(produced, 1);
    assert.deepEqual((await reader.read()).value, new Uint8Array([2]));
    assert.equal((await reader.read()).done, true);
});

test("a stalled network request does not prevent another request completing", async () => {
    let release;
    execute = (url) => url.endsWith("/slow") ? new Promise((resolve) => { release = resolve; }) : Promise.resolve(new Response("ready"));
    const slow = fetch("https://example.test/slow");
    const fast = await fetch("https://example.test/fast");
    assert.equal(await fast.text(), "ready");
    release(new Response("later"));
    assert.equal(await (await slow).text(), "later");
});

test("cancelling a body aborts its HTTP request", async () => {
    let signal;
    execute = async (_, options) => {
        signal = options.signal;
        return new Response(new ReadableStream());
    };
    const response = await fetch("https://example.test/book");
    await response.body.cancel();
    await waitForAbort(signal);
    assert.equal(signal.aborted, true);
});

test("worker failure rejects a pending response body instead of leaving a reader hung", async () => {
    execute = async () => new Response(new ReadableStream());
    const responses = await Promise.all([fetch("https://example.test/first"), fetch("https://example.test/second")]);
    const rejected = Promise.all(responses.map(response => assert.rejects(response.body.getReader().read(), /worker crashed/)));
    policy.failFetchBridge(new Error("worker crashed"));
    await rejected;
});

test("worker failure rejects requests waiting for headers and releases their HTTP work", async () => {
    let started;
    const ready = new Promise(resolve => { started = resolve; });
    let signal;
    execute = (_, options) => {
        signal = options.signal;
        started();
        return new Promise((_, reject) => signal.addEventListener('abort', () => reject(signal.reason), {once: true}));
    };
    const response = fetch('https://example.test/pending-headers');
    const rejected = assert.rejects(response, /worker crashed/);
    await ready;
    policy.failFetchBridge(new Error('worker crashed'));
    await rejected;
    await waitForAbort(signal);
    assert.equal(signal.aborted, true);
});

test("HTTP errors preserve their status and body for existing retry policy", async () => {
    execute = async () => new Response("retry later", { status: 429 });
    const response = await fetch("https://example.test/sync");
    assert.equal(response.status, 429);
    assert.equal(await response.text(), "retry later");
});

test("POST bytes survive browsers without a Request.body stream", async () => {
    const NativeRequest = globalThis.Request;
    globalThis.Request = class extends NativeRequest { get body() { return null; } };
    try {
        execute = async (_, options) => {
            assert.deepEqual([...options.body], [8, 18, 42]);
            return new Response(null, { status: 204 });
        };
        assert.equal((await fetch("https://example.test/sync", { method: "POST", body: new Uint8Array([8, 18, 42]) })).status, 204);
    } finally { globalThis.Request = NativeRequest; }
});

test("an already aborted request never reaches the HTTP worker", async () => {
    execute = () => { throw Error("must not execute"); };
    const controller = new AbortController();
    controller.abort(new Error("cancelled before fetch"));
    await assert.rejects(fetch("https://example.test/book", {signal: controller.signal}), /cancelled before fetch/);
});

test("aborting while waiting for headers rejects fetch and aborts HTTP", async () => {
    let started;
    const ready = new Promise(resolve => { started = resolve; });
    let signal;
    execute = (_, options) => {
        signal = options.signal;
        started();
        return new Promise((_, reject) => signal.addEventListener("abort", () => reject(signal.reason), {once: true}));
    };
    const controller = new AbortController();
    const response = fetch("https://example.test/book", {signal: controller.signal});
    const rejected = assert.rejects(response, /cancelled before headers/);
    await ready;
    controller.abort(new Error("cancelled before headers"));
    await rejected;
    await waitForAbort(signal);
    assert.equal(signal.aborted, true);
});

test("aborting an active body rejects a waiting read", async () => {
    execute = async () => new Response(new ReadableStream());
    const controller = new AbortController();
    const response = await fetch("https://example.test/book", {signal: controller.signal});
    const reading = response.body.getReader().read();
    const rejected = assert.rejects(reading, /cancelled body/);
    controller.abort(new Error("cancelled body"));
    await rejected;
});

test("response URL and redirect metadata survive the worker boundary", async () => {
    execute = async () => {
        const response = new Response("redirected");
        Object.defineProperties(response, {url: {value: "https://example.test/final"}, redirected: {value: true}});
        return response;
    };
    const response = await fetch(new Request("https://example.test/first"));
    assert.equal(response.url, "https://example.test/final");
    assert.equal(response.redirected, true);
    assert.equal(await response.text(), "redirected");
});

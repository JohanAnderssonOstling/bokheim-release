// Isolated tests of the shipped Atomics helpers; no browser or app integration.
import {readFile} from 'node:fs/promises';
import test from 'node:test';
import assert from 'node:assert/strict';
const rust = await readFile(new URL('../src/signal.rs', import.meta.url), 'utf8');
const source = rust.match(/inline_js = r#"([\s\S]*?)"#/)[1];
const signal = await import(`data:text/javascript;base64,${Buffer.from(source).toString('base64')}`);
test('notify increments the generation and wakes asynchronous waiters', async () => {
    const memory = new WebAssembly.Memory({initial: 1, maximum: 1, shared: true});
    signal.checkFileBridge(memory);
    const waiting = signal.waitFileSignalAsync(memory, 0, 0, 1000);
    signal.fileSignal(memory, 0);
    await waiting;
    assert.equal(new Int32Array(memory.buffer)[0], 1);
});
test('blocking waits reject non-workers and preserve observed-generation semantics', () => {
    const memory = new WebAssembly.Memory({initial: 1, maximum: 1, shared: true});
    assert.throws(() => signal.waitFileSignal(memory, 0, 0, 0), /parser worker/);
    globalThis.WorkerGlobalScope = class { static [Symbol.hasInstance](value) { return value === globalThis; } };
    try {
        assert.equal(signal.waitFileSignal(memory, 0, 0, 0), 'timed-out');
        signal.fileSignal(memory, 0);
        assert.equal(signal.waitFileSignal(memory, 0, 0, 0), 'not-equal');
    } finally { delete globalThis.WorkerGlobalScope; }
    assert.throws(() => signal.checkFileBridge(new WebAssembly.Memory({initial: 1})), /shared memory/);
});

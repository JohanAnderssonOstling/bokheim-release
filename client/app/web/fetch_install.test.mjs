import test from 'node:test';
import assert from 'node:assert/strict';
import {policy} from './policy_wasm.mjs';

test('failed fetch installation can be retried, and successful installation is idempotent', () => {
    const original = Object.getOwnPropertyDescriptor(globalThis, 'fetch');
    const native = globalThis.fetch;
    try {
        Object.defineProperty(globalThis, 'fetch', {configurable: true, writable: false, value: native});
        assert.throws(() => policy.installFetchBridge(), /Could not install/);
        assert.equal(globalThis.fetch, native);
        Object.defineProperty(globalThis, 'fetch', {
            configurable: true,
            get() { return native; },
            set() { throw Error('installation rejected'); },
        });
        assert.throws(() => policy.installFetchBridge(), /installation rejected/);
        Object.defineProperty(globalThis, 'fetch', {configurable: true, writable: true, value: native});
        policy.installFetchBridge();
        const installed = globalThis.fetch;
        assert.notEqual(installed, native);
        policy.installFetchBridge();
        assert.equal(globalThis.fetch, installed);
    } finally {
        Object.defineProperty(globalThis, 'fetch', original);
    }
});

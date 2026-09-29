import test from 'node:test';
import assert from 'node:assert/strict';
import {policy} from './policy_wasm.mjs';

test('message ports release callbacks on close, including close inside a callback', async () => {
    assert.equal(await policy.portLifecycleContract(), true);
});

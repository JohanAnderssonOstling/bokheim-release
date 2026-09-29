import test from 'node:test';
import assert from 'node:assert/strict';
import {policy} from './policy_wasm.mjs';

test('executor timers release callbacks on cancellation and completion', async () => {
    assert.equal(await policy.executorTimerContract(), true);
});

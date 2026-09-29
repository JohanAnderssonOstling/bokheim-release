import test from 'node:test';
import assert from 'node:assert/strict';
import {policy} from './policy_wasm.mjs';

test('request cancellation wins before dispatch and before its first poll', async () => {
    assert.equal(await policy.cancelledRequestContract(), true);
});

test('coordinator shutdown drops ready sync work and queued followups', async () => {
    assert.equal(await policy.cancelledSyncContract(), true);
});

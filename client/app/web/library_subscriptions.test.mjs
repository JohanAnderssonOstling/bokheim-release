import test from 'node:test';
import {policy} from './policy_wasm.mjs';

test('library book leases retain cancellation and reconnect classification', () => {
    policy.library_subscription_contract();
});

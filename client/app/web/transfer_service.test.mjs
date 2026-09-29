import assert from 'node:assert/strict';
import { policy } from './policy_wasm.mjs';

function port() {
    return { sent: [], postMessage(data) { this.sent.push(data); }, close() { this.closed = true; }, start() {}, send(data) { this.onmessage({data}); } };
}
const service = policy.transferService(3);
const first = port(), second = port(), other = port(), overflow = port();
service.request('account/library/book', first);
service.request('account/library/book', second);
service.request('account/library/other', other);
assert.deepEqual(first.sent, [{kind:'run'}]);
assert.deepEqual(second.sent, []);
assert.deepEqual(other.sent, [{kind:'run'}]);
service.request('overflow', overflow);
assert.equal(overflow.sent[0].kind, 'error');
first.send({kind:'cancel'});
assert.ok(!first.closed, 'closing one subscriber must retain the shared executor');
first.send({kind:'result', bytes: new Uint8Array([7])});
assert.deepEqual(second.sent, [{kind:'result', bytes:new Uint8Array([7])}]);
assert.equal(service.count, 1);
const retry = port();
service.request('account/library/book', retry);
assert.equal(retry.sent[0].kind, 'run');
service.fail(new Error('storage stopped'));
assert.equal(service.count, 0);
assert.equal(service.job_count(), 0);
assert.equal(other.sent.at(-1).kind, 'error');
const rejected = port();
service.request('later', rejected);
assert.equal(rejected.sent[0].kind, 'error');
console.log('PASS transfer ownership, deduplication, subscriber cancellation, overload, and failure');

// Only the executor can complete a shared transfer, and an old executor cannot
// complete a new transfer after its key has been reused.
const owners = policy.transferService(2);
const owner=port(), subscriber=port();
owners.request('key',owner); owners.request('key',subscriber);
subscriber.send({kind:'result',bytes:new Uint8Array([99])});
assert.equal(owners.count,2);
assert.equal(subscriber.sent.length,0);
owner.send({kind:'result',bytes:new Uint8Array([1])});
assert.equal(owners.count,0);
const replacement=port();owners.request('key',replacement);
assert.equal(owner.closed,true);
assert.ok(owner.onmessage == null);
assert.ok(owner.onmessageerror == null);
assert.equal(owners.count,1);
assert.equal(replacement.closed,undefined);
owners.fail('shutdown');

// A cancelled owner's executor still occupies a job slot even with no waiters.
const bounded = policy.transferService(1);
const cancelled=port();bounded.request('first',cancelled);cancelled.send({kind:'cancel'});
assert.equal(bounded.count,0);
const excess=port();bounded.request('second',excess);
assert.match(excess.sent[0].error,/queue is full/);
cancelled.send({kind:'result'});
const accepted=port();bounded.request('second',accepted);
assert.deepEqual(accepted.sent,[{kind:'run'}]);
bounded.fail('shutdown');

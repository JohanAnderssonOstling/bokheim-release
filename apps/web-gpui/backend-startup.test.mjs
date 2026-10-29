import assert from 'node:assert/strict';
import {test} from 'node:test';
import {prestartBackend} from './backend-startup.mjs';

function deferred() {
    let resolve;
    let reject;
    const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
    return {promise, resolve, reject};
}

test('backend starts before the UI claims it and ownership moves exactly once', async () => {
    const started = deferred();
    const connection = {free() { assert.fail('claimed connection belongs to the UI'); }};
    const calls = [];
    const startup = prestartBackend('./pkg/build', {load: async url => {
        calls.push(url);
        return {
            default: async () => calls.push('coordinator initialized'),
            prepareBackend: async url => { calls.push(url); started.resolve(); return connection; },
        };
    }});
    await started.promise;
    assert.deepEqual(calls, ['./pkg/build/sync_coordinator.js', 'coordinator initialized', './pkg/build/web_backend_worker_loader.js']);
    assert.equal(await startup.take(), connection);
    assert.throws(() => startup.take(), /no longer available/);
    startup.stop();
    startup.stop();
});

test('cancelling while the module downloads does not create a backend', async () => {
    const loaded = deferred();
    const startup = prestartBackend('./pkg/build', {load: () => loaded.promise});
    startup.stop();
    loaded.resolve({default() { assert.fail('cancelled startup must not initialize'); }});
    await new Promise(resolve => setImmediate(resolve));
    assert.throws(() => startup.take(), /no longer available/);
});

test('cancelling during connection creation frees the eventual owner', async () => {
    const started = deferred();
    const connected = deferred();
    let freed = 0;
    const startup = prestartBackend('./pkg/build', {load: async () => ({
        default: async () => {},
        prepareBackend: () => { started.resolve(); return connected.promise; },
    })});
    await started.promise;
    startup.stop();
    connected.resolve({free() { freed++; }});
    await new Promise(resolve => setImmediate(resolve));
    startup.stop();
    assert.equal(freed, 1);
});

test('UI startup failure frees a prepared connection that was never claimed', async () => {
    const started = deferred();
    let freed = 0;
    const startup = prestartBackend('./pkg/build', {load: async () => ({
        default: async () => {},
        prepareBackend: async () => { started.resolve(); return {free() { freed++; }}; },
    })});
    await started.promise;
    await new Promise(resolve => setImmediate(resolve));
    startup.stop();
    startup.stop();
    assert.equal(freed, 1);
});

test('early loading failures propagate when the UI claims the connection', async () => {
    const cause = new Error('coordinator download failed');
    const startup = prestartBackend('./pkg/build', {load: async () => { throw cause; }});
    await new Promise(resolve => setImmediate(resolve));
    await assert.rejects(startup.take(), error => error === cause);
});

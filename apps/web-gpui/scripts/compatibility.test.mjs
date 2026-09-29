import assert from 'node:assert/strict';
import test from 'node:test';
import {browserVersion, requiredCapabilities} from '../compatibility.mjs';

function supported(userAgent = '') {
    return {
        navigator: {userAgent, storage: {getDirectory() {}}, locks: {request() {}}},
        isSecureContext: true, crossOriginIsolated: true,
        Atomics: {waitAsync() {}}, WebAssembly: {}, SharedArrayBuffer: function() {}, Worker: function() {}, SharedWorker: function() {},
    };
}

test('version floors cover desktop, Android, Safari and iOS browser shells', () => {
    for (const [old, current] of [
        ['Chrome/108.0', 'Chrome/109.0'],
        ['Android Chrome/147.0', 'Android Chrome/148.0'],
        ['Firefox/113.0', 'Firefox/114.0'],
        ['Version/16.3 Safari/605.1', 'Version/16.4 Safari/605.1'],
        ['iPhone OS 16_3 CriOS/151.0', 'iPhone OS 16_4 CriOS/151.0'],
    ]) {
        assert.match(requiredCapabilities(supported(old))[0], /too old/, old);
        assert.deepEqual(requiredCapabilities(supported(current)), [], current);
    }
    assert.equal(browserVersion('Unknown browser'), null);
    assert.deepEqual(requiredCapabilities(supported('Unknown browser')), []);
});

test('new browser versions still need storage, workers and correct hosting', () => {
    const env = supported('Android Chrome/151.0');
    env.SharedWorker = undefined;
    env.navigator.storage = undefined;
    env.navigator.locks = undefined;
    env.crossOriginIsolated = false;
    const issues = requiredCapabilities(env).join('\n');
    assert.match(issues, /shared background workers/);
    assert.match(issues, /private file storage/);
    assert.match(issues, /locking/);
    assert.match(issues, /site administrator/);
});

test('media controls are optional, but insecure hosting and missing WASM are explained', () => {
    const env = supported();
    assert.deepEqual(requiredCapabilities(env), []);
    env.isSecureContext = false;
    env.WebAssembly = undefined;
    assert.match(requiredCapabilities(env).join('\n'), /HTTPS/);
    assert.match(requiredCapabilities(env).join('\n'), /WebAssembly/);
});

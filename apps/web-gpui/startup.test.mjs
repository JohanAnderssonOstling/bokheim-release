import assert from 'node:assert/strict';
import {test} from 'node:test';
import {startApplication} from './startup.mjs';

class Element {
    constructor(tagName) {
        this.tagName = tagName;
        this.children = [];
        this.style = {};
        this.listeners = new Map();
    }
    append(...children) { this.children.push(...children); }
    replaceChildren(...children) { this.children = children; }
    addEventListener(event, callback) { this.listeners.set(event, callback); }
    remove() { this.removed = true; }
}

test('failed startup exposes its cause as text and retains manual reload', async t => {
    t.mock.method(console, 'error', () => {});
    const previous = globalThis.document;
    globalThis.document = {createElement: tag => new Element(tag)};
    t.after(() => { globalThis.document = previous; });
    const panel = new Element('main');
    const cause = 'unsupported library schema version 72 <script>example</script>';
    let reloads = 0;
    const result = await startApplication(panel, async () => {
        globalThis.__BOKHEIM_STARTUP_FAILED__(cause);
    }, {storage: null, reload: () => reloads++});

    assert.equal(result, false);
    assert.equal(reloads, 0);
    const [message, details, button] = panel.children;
    assert.equal(message.textContent, 'Bokheim could not start. Reload to try again.');
    assert.equal(details.tagName, 'details');
    assert.equal(details.children[0].textContent, 'Error details');
    assert.equal(details.children[1].textContent, `Error: ${cause}`);
    button.listeners.get('click')();
    assert.equal(reloads, 1);
    assert.equal(globalThis.__BOKHEIM_STARTUP_FAILED__, undefined);
});

test('storage contention retains the instruction to close other tabs', async t => {
    t.mock.method(console, 'error', () => {});
    const previous = globalThis.document;
    globalThis.document = {createElement: tag => new Element(tag)};
    t.after(() => { globalThis.document = previous; });
    const panel = new Element('main');
    const result = await startApplication(panel, async () => {
        throw new Error('BOKHEIM_STORAGE_BUSY');
    }, {storage: null});
    assert.equal(result, false);
    assert.equal(panel.children[0].textContent, 'Close other Bokheim tabs, then reload.');
    assert.match(panel.children[1].children[1].textContent, /BOKHEIM_STORAGE_BUSY/);
});

test('successful startup waits for backend readiness and clears retry state', async () => {
    const panel = new Element('main');
    const removedKeys = [];
    const result = await startApplication(panel, async () => {
        globalThis.__BOKHEIM_STARTUP_READY__();
    }, {storage: {removeItem: key => removedKeys.push(key)}});
    assert.equal(result, true);
    assert.equal(panel.removed, true);
    assert.deepEqual(removedKeys, ['bokheim-startup-retry-v1']);
});

test('UI loading failure releases its early backend before retrying', async t => {
    t.mock.method(console, 'error', () => {});
    const panel = new Element('main');
    const events = [];
    assert.equal(await startApplication(panel, async () => { throw new Error('UI download failed'); }, {
        storage: {getItem: () => null, setItem() {}},
        onFailure: () => events.push('backend released'),
        reload: () => events.push('reload'),
    }), false);
    assert.deepEqual(events, ['backend released', 'reload']);
});

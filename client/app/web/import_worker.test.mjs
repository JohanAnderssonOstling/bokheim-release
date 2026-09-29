import assert from 'node:assert/strict';
import {test} from 'node:test';
import {openImport, discoverImports} from '../../../apps/web-backend-worker/import_worker.js';
import {readFileSync} from 'node:fs';
import {dirname, resolve} from 'node:path';
import {fileURLToPath, pathToFileURL} from 'node:url';

const output = resolve(process.env.COORDINATOR_TEST_OUTPUT || resolve(dirname(fileURLToPath(import.meta.url)), '../../.test-tmp/coordinator-policy-wasm'), 'cpu_worker');
globalThis.postMessage = () => {};
const {default: initialize, ImportHasher, recover_import, import_workflow_contract} = await import(pathToFileURL(resolve(output, 'cpu_worker.js')));
await initialize({module_or_path: readFileSync(resolve(output, 'cpu_worker_bg.wasm'))});

// The mock database is a real MessagePort peer of the Rust transport.
const runImport = async (job, dependencies) => import_workflow_contract(job, await openImport(dependencies.storage, job),
    async command => JSON.stringify(await dependencies.request(job.library, JSON.parse(command)) ?? null), dependencies.progress);
const digest = bytes => { const hasher = new ImportHasher(); hasher.update(bytes); return hasher.finish(); };

function memoryStorage() {
    const files = new Map();
    let writes = 0;
    const opened = new Set();
    let fault;
    const directory = prefix => ({
        async getDirectoryHandle(name) { return directory(`${prefix}${name}/`); },
        async getFileHandle(name) {
            const key = prefix + name;
            return {async createSyncAccessHandle() {
                assert.ok(!opened.has(key), `handle already open: ${key}`);
                opened.add(key);
                if (!files.has(key)) files.set(key, new Uint8Array());
                return {
                    getSize: () => files.get(key).length,
                    read(bytes, {at = 0} = {}) { const data = files.get(key).subarray(at, at + bytes.length); bytes.set(data); return data.length; },
                    write(bytes, {at = 0} = {}) {
                        writes++;
                        fault?.(key, writes);
                        const before = files.get(key);
                        const data = new Uint8Array(Math.max(before.length, at + bytes.length));
                        data.set(before); data.set(bytes, at); files.set(key, data); return bytes.length;
                    },
                    truncate(size) { files.set(key, files.get(key).slice(0, size)); },
                    flush() {}, close() { opened.delete(key); },
                };
            }};
        },
    });
    return {getDirectory: async () => directory(''), files, opened, setFault: value => fault = value};
}
function fixture() {
    const storage = memoryStorage();
    const id = '12345678-1234-1234-1234-123456789012';
    const files = [new File(['one'], 'a.epub'), new File(['second book'], 'b.epub')];
    const job = {id, library: '00000000-0000-0000-0000-000000000123', parent: '00000000-0000-0000-0000-000000000000', name: 'Books', createRoot: true, directories: [['Shelf'], ['Empty']], files: files.map(file => ({path: ['Shelf', file.name], file}))};
    const progress = [];
    const requests = [];
    let fail;
    const dependencies = {storage, progress: value => progress.push(value), request: async (_library, command) => {
        requests.push(command);
        fail?.(command);
        for (let i = 0; i < 2; i++) assert.deepEqual(storage.files.get(`__libraries/${job.library}/imports/${id}/${i}`), new Uint8Array(await files[i].arrayBuffer()), 'Entire folder must be copied before any database request');
        if (command.PrepareStagedDirectoryImport) return {directories: [[['Shelf'], '11111111-1111-1111-1111-111111111111'], [['Empty'], '22222222-2222-2222-2222-222222222222']], failures: []};
        if (command.ImportStagedBook) {
            const entry = command.ImportStagedBook;
            assert.equal(entry.hash, digest(storage.files.get(entry.physical)));
            assert.equal(entry.length, storage.files.get(entry.physical).length);
            assert.equal(entry.parent_id, '11111111-1111-1111-1111-111111111111');
            return entry.hash;
        }
    }};
    return {job, dependencies, progress, requests, storage, setRequestFailure: value => fail = value};
}
test('copies and hashes the whole tree before metadata, emits both progress phases', async () => {
    const f = fixture();
    assert.deepEqual(await runImport(f.job, f.dependencies), []);
    assert.deepEqual(f.requests[0].PrepareStagedDirectoryImport.directories, [['Shelf'], ['Empty']]);
    const activity = f.requests[0].PrepareStagedDirectoryImport.activity;
    assert.match(activity, /^[0-9a-f-]{36}$/);
    for (const request of f.requests) {
        const update = request.AdvanceDirectoryImport || request.FinishDirectoryImport;
        if (update) assert.equal(update.activity, activity, 'progress and completion belong to the prepared activity');
    }
    assert.ok(f.progress.some(p => p.phase === 'copying' && p.copiedBytes === 14 && p.copiedFiles === 2));
    assert.ok(f.progress.some(p => p.phase === 'adding' && p.processed === 2));
    assert.equal(f.progress.at(-1).phase, 'complete');
});
test('noncanonical copied identities fail before any book metadata request', async () => {
    const f = fixture();
    const files = f.job.files.map(({path, file}) => ({path, size: file.size, modified: file.lastModified}));
    const encoder = new TextEncoder();
    f.storage.files.set(`__libraries/${f.job.library}/imports/${f.job.id}/manifest`, encoder.encode(JSON.stringify({...f.job, files})));
    f.storage.files.set(`__libraries/${f.job.library}/imports/${f.job.id}/journal`, encoder.encode(files.map((_, index) => JSON.stringify({kind: 'copied', index, hash: 'A'.repeat(64)}) + '\n').join('')));
    for (let index = 0; index < files.length; index++) f.storage.files.set(`__libraries/${f.job.library}/imports/${f.job.id}/${index}`, new Uint8Array(await f.job.files[index].file.arrayBuffer()));
    const failures = await runImport(f.job, f.dependencies);
    assert.equal(failures.length, 2);
    assert.deepEqual(failures.map(failure => [failure.kind, failure.path]), [['file', ['Shelf', 'a.epub']], ['file', ['Shelf', 'b.epub']]]);
    assert.equal(f.requests.filter(command => command.ImportStagedBook).length, 0);
});

test('copy interruption inserts nothing; replacement retries unfinished file and skips completed copies', async () => {
    const f = fixture();
    let firstCopies = 0;
    f.storage.setFault(key => {
        if (key.endsWith('/0')) firstCopies++;
        if (key.endsWith('/1')) throw Error('Worker terminated');
    });
    await assert.rejects(runImport(f.job, f.dependencies), /terminated/);
    assert.equal(f.requests.length, 0);
    f.storage.setFault(key => { if (key.endsWith('/0')) firstCopies++; });
    f.progress.length = 0;
    await runImport(f.job, f.dependencies);
    assert.equal(firstCopies, 1);
    assert.equal(f.progress[0].copiedBytes, 3, 'resumed progress includes only the checkpointed first file');
    assert.equal(f.progress[0].copiedFiles, 1);
    assert.equal(f.progress[0].totalBytes, 14);
});
test('a fully copied job resumes without source File objects; acknowledged books are skipped', async () => {
    const f = fixture();
    f.setRequestFailure(command => { if (command.ImportStagedBook?.file_name === 'b.epub') throw Object.assign(Error('host lost'), {transport: true}); });
    await assert.rejects(runImport(f.job, f.dependencies), /host lost/);
    const previousActivity = f.requests[0].PrepareStagedDirectoryImport.activity;
    f.setRequestFailure(null);
    f.requests.length = 0;
    await runImport({...f.job, files: []}, f.dependencies);
    assert.notEqual(f.requests[0].PrepareStagedDirectoryImport.activity, previousActivity, 'recovery owns a new activity, so a late completion cannot finish it');
    assert.deepEqual(f.requests.filter(r => r.ImportStagedBook).map(r => r.ImportStagedBook.file_name), ['b.epub']);
    assert.deepEqual(f.requests[1].AdvanceDirectoryImport, {activity: f.requests[0].PrepareStagedDirectoryImport.activity, total: 2, succeeded: 1, failed: 0});
});
test('per-book failures continue and batched progress flushes before finishing', async t => {
    t.mock.method(performance, 'now', () => 0);
    const f = fixture();
    f.setRequestFailure(command => { if (command.ImportStagedBook?.file_name === 'a.epub') throw Error('Invalid EPUB'); });
    const failures = await runImport(f.job, f.dependencies);
    assert.equal(failures.length, 1);
    assert.equal(failures[0].kind, 'file');
    assert.deepEqual(failures[0].path, ['Shelf', 'a.epub']);
    assert.equal(f.progress.at(-1).failed, 1);
    assert.equal(f.progress.at(-1).processed, 2);
    const updates = f.requests.flatMap(command => command.AdvanceDirectoryImport ? [command.AdvanceDirectoryImport] : []);
    assert.equal(updates.length, 2, 'fast files share one final update after the initial total');
    assert.deepEqual(updates.reduce((sum, update) => ({total: sum.total + update.total, succeeded: sum.succeeded + update.succeeded, failed: sum.failed + update.failed}), {total: 0, succeeded: 0, failed: 0}), {total: 2, succeeded: 1, failed: 1});
    assert.deepEqual(f.requests.at(-2), {FinishDirectoryImport: {activity: f.requests[0].PrepareStagedDirectoryImport.activity}});
});

test('longer imports report intermediate progress without repeating the final counts', async t => {
    let now = 0;
    t.mock.method(performance, 'now', () => now);
    const f = fixture();
    f.setRequestFailure(command => { if (command.ImportStagedBook) now += 250; });
    await runImport(f.job, f.dependencies);
    const activity = f.requests[0].PrepareStagedDirectoryImport.activity;
    assert.deepEqual(f.requests.flatMap(command => command.AdvanceDirectoryImport ? [command.AdvanceDirectoryImport] : []), [
        {activity, total: 2, succeeded: 0, failed: 0},
        {activity, total: 0, succeeded: 1, failed: 0},
        {activity, total: 0, succeeded: 1, failed: 0},
    ]);
});

test('a torn checkpoint is discarded before retry and completion prevents replaying metadata', async () => {
    const f = fixture();
    f.setRequestFailure(command => {
        if (command.ImportStagedBook?.file_name === 'b.epub') throw Object.assign(Error('host lost'), {transport: true});
    });
    await assert.rejects(runImport(f.job, f.dependencies), /host lost/);
    const key = `__libraries/${f.job.library}/imports/${f.job.id}/journal`;
    const committed = f.storage.files.get(key);
    const torn = new Uint8Array(committed.length + 2);
    torn.set(committed); torn.set([123, 195], committed.length);
    f.storage.files.set(key, torn);
    f.setRequestFailure(null);
    assert.deepEqual(await runImport({...f.job, files: []}, f.dependencies), []);
    f.requests.length = 0;
    assert.deepEqual(await runImport({...f.job, files: []}, f.dependencies), []);
    assert.equal(f.requests.length, 0, 'completed imports do not repeat metadata requests');
    assert.equal(f.progress.at(-1).phase, 'complete');
    assert.equal(f.progress.at(-1).processed, 2);
});

test('discovery uses checkpoint replay to omit completed imports and recover torn tails', async () => {
    const encoder = new TextEncoder();
    const jobs = [
        ['complete', '{"kind":"complete","failures":[]}\n'],
        ['pending', '{"kind":"copied","index":0,"hash":"copied"}\n{"kind":'],
    ];
    const directory = {async *entries() {
        for (const [id, journal] of jobs) yield [id, {kind: 'directory', async getFileHandle(name) {
            return {async getFile() {
                const text = name === 'journal' ? journal : JSON.stringify({...fixture().job, id, files: []});
                return {text: async () => text, arrayBuffer: async () => encoder.encode(text).buffer};
            }};
        }}];
    }};
    const storage = {getDirectory: async () => ({getDirectoryHandle: async () => ({async *entries() {
        yield [fixture().job.library, {kind: 'directory', getDirectoryHandle: async () => directory}];
    }})})};
    const entries = await discoverImports(storage);
    const recovered = entries.map(entry => recover_import(entry.manifest, entry.journal)).filter(Boolean);
    assert.deepEqual(recovered.map(job => job.id), ['pending']);
});

test('failed checkpoint writes discard live state and retry the unacknowledged copy', async () => {
    const f = fixture();
    let copies = 0;
    let failCheckpoint = true;
    f.storage.setFault(key => {
        if (key.endsWith('/0')) copies++;
        if (key.endsWith('/journal') && failCheckpoint) throw Error('checkpoint storage failed');
    });
    await assert.rejects(runImport(f.job, f.dependencies), /checkpoint storage failed/);
    assert.equal(f.requests.length, 0);
    failCheckpoint = false;
    assert.deepEqual(await runImport(f.job, f.dependencies), []);
    assert.equal(copies, 2, 'only a durable checkpoint can suppress copying on retry');
});

test('Rust destination planning reports missing parents while importing other files', async () => {
    const f = fixture();
    f.job.files[0].path = ['Missing', 'a.epub'];
    const failures = await runImport(f.job, f.dependencies);
    assert.equal(failures.length, 1);
    assert.equal(failures[0].kind, 'file');
    assert.deepEqual(failures[0].path, ['Missing', 'a.epub']);
    assert.deepEqual(f.requests.filter(command => command.ImportStagedBook).map(command => command.ImportStagedBook.file_name), ['b.epub']);
    assert.equal(f.progress.at(-1).failed, 1);
    assert.equal(f.progress.at(-1).processed, 2);
});

test('an empty folder import still prepares directories and finishes its activity', async () => {
    const storage = memoryStorage();
    const requests = [];
    const progress = [];
    const job = {id: '12345678-1234-1234-1234-123456789012', library: '00000000-0000-0000-0000-000000000123', parent: '00000000-0000-0000-0000-000000000000', name: 'Empty', createRoot: true, directories: [[]], files: []};
    const failures = await runImport(job, {storage, progress: value => progress.push(value), request: async (_library, command) => {
        requests.push(command);
        if (command.PrepareStagedDirectoryImport) return {directories: [[[], job.parent]], failures: []};
    }});
    assert.deepEqual(failures, []);
    assert.equal(requests[0].PrepareStagedDirectoryImport.name, 'Empty');
    assert.deepEqual(requests[1].AdvanceDirectoryImport, {activity: requests[0].PrepareStagedDirectoryImport.activity, total: 0, succeeded: 0, failed: 0});
    assert.deepEqual(requests[2], {FinishDirectoryImport: {activity: requests[0].PrepareStagedDirectoryImport.activity}});
    assert.equal(requests[3].CleanupStagedImport.files, 0);
    assert.equal(progress.at(-1).phase, 'complete');
    assert.equal(progress.at(-1).totalFiles, 0);
});

test('a torn initial manifest is rebuilt from retained sources and all handles close', async () => {
    const f = fixture();
    f.storage.files.set(`__libraries/${f.job.library}/imports/${f.job.id}/manifest`, new TextEncoder().encode('{"id":'));
    assert.deepEqual(await runImport(f.job, f.dependencies), []);
    assert.equal(f.storage.opened.size, 0);
});

test('missing or changed sources fail before metadata and release all access handles', async () => {
    for (const changed of [false, true]) {
        const f = fixture();
        f.storage.setFault(key => { if (key.endsWith('/0')) throw Error('interrupted'); });
        await assert.rejects(runImport(f.job, f.dependencies), /interrupted/);
        assert.equal(f.storage.opened.size, 0);
        f.storage.setFault(null);
        if (changed) f.job.files[0].file = new File(['changed source'], 'a.epub');
        else delete f.job.files[0].file;
        await assert.rejects(runImport(f.job, f.dependencies), /selected again/);
        assert.equal(f.requests.length, 0);
        assert.equal(f.storage.opened.size, 0);
    }
});

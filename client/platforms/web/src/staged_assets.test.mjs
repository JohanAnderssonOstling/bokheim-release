import assert from 'node:assert/strict';
import {test} from 'node:test';
import {closeStaged, createAsset, listAssets, removeLibrary} from './staged_assets.js';

class Directory {
    kind = 'directory';
    entriesByName = new Map();

    async getDirectoryHandle(name, {create = false} = {}) {
        let entry = this.entriesByName.get(name);
        if (!entry && create) this.entriesByName.set(name, entry = new Directory());
        if (!entry) throw Object.assign(Error('Missing directory'), {name: 'NotFoundError'});
        if (entry.kind !== 'directory') throw Error('Not a directory');
        return entry;
    }

    async getFileHandle(name, {create = false} = {}) {
        let entry = this.entriesByName.get(name);
        if (!entry && create) this.entriesByName.set(name, entry = {
            kind: 'file',
            async createSyncAccessHandle() { return {truncate() {}, close() {}}; },
        });
        if (!entry) throw Object.assign(Error('Missing file'), {name: 'NotFoundError'});
        return entry;
    }

    async removeEntry(name, {recursive = false} = {}) {
        const entry = this.entriesByName.get(name);
        if (!entry) throw Object.assign(Error('Missing entry'), {name: 'NotFoundError'});
        if (entry.kind === 'directory' && entry.entriesByName.size && !recursive) throw Error('Directory is not empty');
        this.entriesByName.delete(name);
    }

    async *entries() { yield* this.entriesByName; }
    async *keys() { yield* this.entriesByName.keys(); }
}

test('library purge removes only its physical container', async () => {
    const root = new Directory();
    Object.defineProperty(globalThis, 'navigator', {configurable: true, value: {storage: {getDirectory: async () => root}}});
    const first = '11111111-1111-1111-1111-111111111111';
    const second = '22222222-2222-2222-2222-222222222222';
    const firstBook = `__libraries/${first}/book/aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa`;
    const firstCover = `__libraries/${first}/thumbnail/bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb`;
    const secondBook = `__libraries/${second}/book/cccccccc-cccc-cccc-cccc-cccccccccccc`;
    for (const name of [firstBook, firstCover, secondBook]) {
        await createAsset(name);
        closeStaged(name);
    }

    assert.deepEqual((await listAssets()).sort(), [firstBook, firstCover, secondBook].sort());
    await removeLibrary(first);
    assert.deepEqual(await listAssets(), [secondBook]);
    await removeLibrary(first); // Interrupted deletions are safe to replay.
    await assert.rejects(createAsset('__book_objects/old'), /Invalid staged file path/);
});

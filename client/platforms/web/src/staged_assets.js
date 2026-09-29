// Asset files outside SQLite's handle pool. Rust owns book and lifetimes.
const handles = new Map();
const opening = new Map();
async function stagedFile(name, create = false) {
    if (!/^__libraries\/[0-9a-f-]{36}\/(?:imports\/[0-9a-f-]{36}\/\d+|(?:book|thumbnail)\/[0-9a-f-]{36})$/.test(name)) throw Error('Invalid staged file path');
    const parts = name.split('/');
    let directory = await navigator.storage.getDirectory();
    for (const component of parts.slice(0, -1)) directory = await directory.getDirectoryHandle(component, {create});
    return directory.getFileHandle(parts.at(-1), {create});
}
export async function openStaged(name) {
    if (handles.has(name)) return handles.get(name).getSize();
    if (!opening.has(name)) opening.set(name, (async () => {
        const handle = await (await stagedFile(name)).createSyncAccessHandle();
        handles.set(name, handle);
        return handle.getSize();
    })().finally(() => opening.delete(name)));
    return opening.get(name);
}
export function closeStaged(name) {
    handles.get(name)?.close();
    handles.delete(name);
}
export function readStaged(name, offset, bytes) { return handles.get(name).read(bytes, {at: offset}); }
export function retireStaged(name) {
    closeStaged(name);
    // Deletion follows index book; failure can be retried on cleanup.
    (async () => {
        let directory = await navigator.storage.getDirectory();
        for (const component of name.split('/').slice(0, -1)) directory = await directory.getDirectoryHandle(component);
        await directory.removeEntry(name.split('/').at(-1));
    })().catch(error => { if (error.name !== 'NotFoundError') console.warn('Could not remove retired asset file', error); });
}

export async function createAsset(name) {
    const handle = await (await stagedFile(name, true)).createSyncAccessHandle();
    handles.set(name, handle);
    handle.truncate(0);
}
export function writeStaged(name, offset, bytes) {
    const count = handles.get(name).write(bytes, {at: offset});
    if (count !== bytes.length) throw Error('Incomplete asset write');
}
export function flushStaged(name) { handles.get(name).flush(); }

export async function listAssets() {
    const root = await navigator.storage.getDirectory();
    const names = [];
    let libraries;
    try { libraries = await root.getDirectoryHandle('__libraries'); }
    catch (error) { if (error.name === 'NotFoundError') return names; throw error; }
    for await (const [id, library] of libraries.entries()) {
        if (library.kind !== 'directory') continue;
        for (const kind of ['book', 'thumbnail']) {
            let directory;
            try { directory = await library.getDirectoryHandle(kind); }
            catch (error) { if (error.name === 'NotFoundError') continue; throw error; }
            for await (const name of directory.keys()) names.push(`__libraries/${id}/${kind}/${name}`);
        }
    }
    return names;
}
export async function removeAsset(name) {
    if (!/^__libraries\/[0-9a-f-]{36}\/(?:imports\/[0-9a-f-]{36}\/\d+|(?:book|thumbnail)\/[0-9a-f-]{36})$/.test(name)) throw Error('Invalid asset file path');
    const parts = name.split('/');
    try {
        let directory = await navigator.storage.getDirectory();
        for (const part of parts.slice(0, -1)) directory = await directory.getDirectoryHandle(part);
        await directory.removeEntry(parts.at(-1));
    } catch (error) {
        // A reader retirement can remove the same orphan during enumeration.
        if (error.name !== 'NotFoundError') throw error;
    }
}

export async function removeLibrary(library) {
    if (!/^[0-9a-f-]{36}$/.test(library)) throw Error('Invalid library id');
    const prefix = `__libraries/${library}/`;
    for (const [name, handle] of handles) {
        if (name.startsWith(prefix)) {
            handle.close();
            handles.delete(name);
        }
    }
    try {
        const root = await navigator.storage.getDirectory();
        const libraries = await root.getDirectoryHandle('__libraries');
        await libraries.removeEntry(library, {recursive: true});
    } catch (error) {
        if (error.name !== 'NotFoundError') throw error;
    }
}

export async function stagedBlob(name) { return (await stagedFile(name)).getFile(); }

import {directoryIo, readFileBytes} from '../../client/platforms/web/import_io.js';
// Browser file capabilities. Rust owns import ordering, hashing and recovery.
export async function openImport(storage, job) {
    const root = await storage.getDirectory();
    if (!/^[0-9a-f-]{36}$/.test(job.library) || !/^[0-9a-f-]{36}$/.test(job.id)) throw Error('Invalid import owner');
    const libraries = await root.getDirectoryHandle('__libraries', {create: true});
    const library = await libraries.getDirectoryHandle(job.library, {create: true});
    const imports = await library.getDirectoryHandle('imports', {create: true});
    const directory = await imports.getDirectoryHandle(job.id, {create: true});
    return directoryIo(directory, job.files);
}

export async function discoverImports(storage) {
    const root = await storage.getDirectory();
    let libraries;
    try { libraries = await root.getDirectoryHandle('__libraries'); }
    catch (error) { if (error.name === 'NotFoundError') return []; throw error; }
    const entries = [];
    for await (const [libraryId, library] of libraries.entries()) {
        if (library.kind !== 'directory' || !/^[0-9a-f-]{36}$/.test(libraryId)) continue;
        let imports;
        try { imports = await library.getDirectoryHandle('imports'); }
        catch (error) { if (error.name === 'NotFoundError') continue; throw error; }
        for await (const [id, folder] of imports.entries()) {
            if (folder.kind !== 'directory') continue;
            try {
                const read = name => readFileBytes(folder, name);
                entries.push({id, journal: await read('journal'), manifest: await read('manifest')});
            } catch (error) { console.warn('Could not read import', id, error); }
        }
    }
    return entries;
}

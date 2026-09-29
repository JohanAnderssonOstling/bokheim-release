// Browser file capabilities; callers choose directories and source files.
export function directoryIo(directory, files) {
    return {
        source: index => files?.[index]?.file,
        async readSource(index, start, end) {
            return new Uint8Array(await files[index].file.slice(start, end).arrayBuffer());
        },
        async open(name) {
            const handle = await (await directory.getFileHandle(name, {create: true})).createSyncAccessHandle();
            return {
                read() {
                    const bytes = new Uint8Array(handle.getSize());
                    if (handle.read(bytes) !== bytes.length) throw Error('Short import read');
                    return bytes;
                },
                write: (bytes, offset) => handle.write(bytes, {at: offset}),
                truncate: size => handle.truncate(size),
                flush: () => handle.flush(),
                close: () => handle.close(),
            };
        },
    };
}

export async function readFileBytes(directory, name) {
    return new Uint8Array(await (await (await directory.getFileHandle(name)).getFile()).arrayBuffer());
}

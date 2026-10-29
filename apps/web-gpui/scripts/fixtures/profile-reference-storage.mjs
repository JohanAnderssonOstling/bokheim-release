// Diagnostic only: preserve browser storage behavior while counting operations.
const handles = new WeakMap();
const totals = new Map();
const events = [];
const decoder = new TextDecoder();
globalThis.__storageProfile = {totals, events};
const bytes = value => ArrayBuffer.isView(value)
    ? new Uint8Array(value.buffer, value.byteOffset, value.byteLength)
    : new Uint8Array(value);
const logicalName = value => {
    const view = bytes(value).subarray(0, 512);
    const end = view.indexOf(0);
    return decoder.decode(view.subarray(0, end < 0 ? view.length : end));
};
const originalOpen = FileSystemFileHandle.prototype.createSyncAccessHandle;
FileSystemFileHandle.prototype.createSyncAccessHandle = async function (...args) {
    const begin = performance.now();
    const handle = await originalOpen.apply(this, args);
    handles.set(handle, {physical: this.name, logical: '(pool)'});
    events.push({operation: 'open', start: begin, ms: performance.now() - begin});
    return handle;
};
for (const operation of ['read', 'write', 'flush', 'truncate']) {
    const original = FileSystemSyncAccessHandle.prototype[operation];
    FileSystemSyncAccessHandle.prototype[operation] = function (...args) {
        const handle = handles.get(this) ?? {logical: '(unknown)'};
        const header = ['read', 'write'].includes(operation) && (args[1]?.at ?? 0) === 0 && args[0].byteLength === 516;
        if (header && operation === 'write') handle.logical = logicalName(args[0]) || '(pool)';
        const begin = performance.now();
        const result = original.apply(this, args);
        const ms = performance.now() - begin;
        if (header && operation === 'read') handle.logical = logicalName(args[0]) || '(pool)';
        const key = `${handle.logical}:${operation}`;
        const entry = totals.get(key) ?? {file: handle.logical, operation, calls: 0, bytes: 0, ms: 0};
        entry.calls++;
        entry.ms += ms;
        if (operation === 'write' || operation === 'read') entry.bytes += result;
        totals.set(key, entry);
        return result;
    };
}
const originalFetch = fetch;
globalThis.fetch = async (...args) => {
    const start = performance.now();
    const result = await originalFetch(...args);
    events.push({operation: 'fetch', url: String(args[0]), start, ms: performance.now() - start});
    return result;
};

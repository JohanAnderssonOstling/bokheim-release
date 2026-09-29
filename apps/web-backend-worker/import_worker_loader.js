import initializeWasm, {start_import_worker} from './cpu_worker.js';
import {openImport, discoverImports} from './import_worker.js';
import {runWorkerEndpoint} from './worker_endpoint.js';

await runWorkerEndpoint(async () => {
    await initializeWasm();
    start_import_worker(
        job => openImport(navigator.storage, job),
        () => discoverImports(navigator.storage),
    );
});

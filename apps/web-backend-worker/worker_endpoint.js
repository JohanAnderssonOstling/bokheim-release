// Application policy: the database worker holds the exclusive storage-owner lock.
import {runWorkerEndpoint as attachWorkerEndpoint} from './worker_transport.js';
export function runWorkerEndpoint(initialize, database = false) {
    return attachWorkerEndpoint(initialize, database ? 'bokheim-database-owner-v1' : null);
}

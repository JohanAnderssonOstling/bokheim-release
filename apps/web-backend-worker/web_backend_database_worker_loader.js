import initialize from "./web_backend_worker.js";
import {runWorkerEndpoint} from "./worker_endpoint.js";

// Rust owns connections and diagnostics once WASM is available.
const waiting = [];
self.onmessage = ({data}) => waiting.push(data);
try {
    await runWorkerEndpoint(initialize, true);
    for (const data of waiting.splice(0)) self.onmessage({data});
} catch (error) {
    const detail = error?.stack ?? error?.message ?? String(error);
    for (const data of waiting.splice(0)) {
        if (data?.transport !== "connect") continue;
        data.port.postMessage({kind:"failed",error:detail});
        data.port.close();
    }
    self.postMessage({transport:"backend_failed",error:detail});
    self.close();
}

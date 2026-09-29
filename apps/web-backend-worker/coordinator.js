// Buffer browser connections until WASM is loaded. Application readiness and
// failure policy belong to Rust once its runtime is available.
const waiting = [];
self.onconnect = ({ports:[port]}) => waiting.push(port);
try {
    const module = await import("./sync_coordinator.js");
    await module.default();
    module.startSharedCoordinator();
    for (const port of waiting.splice(0)) self.onconnect({ports:[port]});
} catch(error) {
    for (const port of waiting.splice(0)) {
        port.postMessage({kind:"failed",error:String(error)});
        port.close();
    }
    self.close();
}

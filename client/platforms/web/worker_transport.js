// Adapt the dedicated worker's existing global-message transport to a direct
// coordinator port. No application requests pass through the tab's UI thread.
export function runWorkerEndpoint(initialize, lifetimeLock = null) {
    if (!new URL(self.location.href).searchParams.has('endpoint')) {
        return initialize();
    }
    return new Promise((resolve, reject) => {
        self.onmessage = ({data}) => {
            if (data?.transport !== 'attach_endpoint') return;
            const port = data.port;
            self.onmessage = null;
            self.postMessage = (message, transfer = []) => port.postMessage(message, transfer);
            const waiting = [];
            port.onmessage = event => waiting.push(event.data);
            port.start();
            const start = async () => {
                await initialize();
                port.onmessage = ({data}) => self.dispatchEvent(new MessageEvent('message', {data}));
                for (const data of waiting) self.dispatchEvent(new MessageEvent('message', {data}));
                waiting.length = 0;
                resolve();
                // Ownership lasts until the worker is terminated. Never
                // steal this lock when replacing a host.
                if (lifetimeLock) await new Promise(() => {});
            };
            const started = lifetimeLock
                ? navigator.locks.request(lifetimeLock, {mode: 'exclusive', ifAvailable: true}, lock => {
                    if (!lock) throw new Error('BOKHEIM_STORAGE_BUSY');
                    return start();
                })
                : start();
            started.catch(error => {
                port.postMessage({transport: 'endpoint_failed', error: String(error)});
                reject(error);
                self.close();
            });
        };
    });
}

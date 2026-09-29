// Version floors are a compatibility policy, not a guarantee that a device can
// run the app. Probe the required capabilities before loading the WASM bundle.
export function browserVersion(userAgent) {
    let match;
    // iOS browser brands normally describe a WebKit shell, not desktop Chrome
    // or Firefox. Use the OS version for these shells.
    if (/iPhone|iPad|iPod/.test(userAgent) && (match = userAgent.match(/OS (\d+)[._](\d+)/))) {
        return {name: 'iOS browser', version: `${match[1]}.${match[2]}`, minimum: '16.4'};
    }
    if ((match = userAgent.match(/Firefox\/(\d+)/))) {
        return {name: 'Firefox', version: match[1], minimum: '114'};
    }
    if ((match = userAgent.match(/(?:Chrome|Chromium)\/(\d+)/))) {
        const android = /Android/.test(userAgent);
        return {name: android ? 'Chrome for Android' : 'Chrome / Chromium', version: match[1], minimum: android ? '148' : '109'};
    }
    if (/Safari\//.test(userAgent) && (match = userAgent.match(/Version\/(\d+(?:\.\d+)?)/))) {
        return {name: 'Safari', version: match[1], minimum: '16.4'};
    }
    return null;
}

function belowMinimum(browser) {
    const [major, minor = 0] = browser.version.split('.').map(Number);
    const [requiredMajor, requiredMinor = 0] = browser.minimum.split('.').map(Number);
    return major < requiredMajor || (major === requiredMajor && minor < requiredMinor);
}

export function compatibilityMessage(userAgent) {
    const browser = browserVersion(userAgent);
    if (browser && belowMinimum(browser)) {
        return `Bokheim requires ${browser.name} ${browser.minimum} or newer.`;
    }
    return 'Update your browser to its latest version.';
}

export function requiredCapabilities(env = globalThis) {
    const issues = [];
    const browser = browserVersion(env.navigator?.userAgent || '');
    if (browser) {
        if (belowMinimum(browser)) {
            issues.push(`${browser.name} ${browser.version} is too old. Update to version ${browser.minimum} or newer.`);
        }
    }
    if (!env.isSecureContext) issues.push('Open Bokheim over HTTPS (or localhost for local testing).');
    if (!env.crossOriginIsolated) issues.push('This site is missing the cross-origin isolation required by Bokheim. The site administrator needs to check its COOP and COEP headers.');
    if (!env.WebAssembly || typeof env.SharedArrayBuffer !== 'function') issues.push('This browser cannot provide the WebAssembly shared memory required by Bokheim.');
    if (typeof env.Atomics?.waitAsync !== 'function') issues.push('This browser cannot wait asynchronously for shared-memory work. Update your browser.');
    if (typeof env.Worker !== 'function') issues.push('This browser does not support background workers.');
    if (typeof env.SharedWorker !== 'function') issues.push('This browser does not support shared background workers. On Android, Chrome 148 or newer is required.');
    if (typeof env.navigator?.storage?.getDirectory !== 'function') issues.push('This browser does not provide the private file storage required for your library.');
    if (typeof env.navigator?.locks?.request !== 'function') issues.push('This browser does not provide the locking required to safely share a library between tabs.');
    return issues;
}

// Confirm that a shared module worker can actually load under this site's CSP.
// Dedicated workers are created by tabs, so no nested-worker probe is needed.
export function checkSharedWorkerSupport() {
    return new Promise(resolve => {
        let worker;
        let url;
        let timer;
        const finish = issue => {
            clearTimeout(timer);
            if (worker) {
                worker.port.onmessage = null;
                worker.onerror = null;
                worker.port.close();
            }
            if (url) URL.revokeObjectURL(url);
            resolve(issue);
        };
        try {
            url = URL.createObjectURL(new Blob([
                'self.onconnect = ({ports:[port]}) => { port.postMessage(true); self.close(); };',
            ], {type: 'text/javascript'}));
            worker = new SharedWorker(url, {type: 'module'});
            timer = setTimeout(() => finish('The browser could not start Bokheim’s shared background worker. Check browser restrictions and reload.'), 10000);
            worker.onerror = event => {
                event.preventDefault();
                finish('The browser could not load a shared module worker. Update your browser or check whether this site blocks workers.');
            };
            worker.port.onmessage = () => finish(null);
            worker.port.start();
        } catch {
            finish('This browser or this site’s security settings prevent Bokheim from starting shared workers.');
        }
    });
}

export async function checkCompatibility() {
    const issues = requiredCapabilities();
    if (issues.length) return issues;
    const workerIssue = await checkSharedWorkerSupport();
    return workerIssue ? [workerIssue] : [];
}

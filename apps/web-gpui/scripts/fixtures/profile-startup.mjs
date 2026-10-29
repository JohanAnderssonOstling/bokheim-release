(() => {
    const state = globalThis.__startupBenchmark = {events: [], errors: [], wasmModules: [], createdWorkers: []};
    const mark = (name, milliseconds = performance.now()) => {
        if (state[name] === undefined) {
            state[name] = milliseconds;
            state.events.push({name, ms: state[name]});
        }
    };
    let paintScheduled = false;
    function maybePaint() {
        if (paintScheduled || state.ready === undefined || state.panelRemoved === undefined || state.firstDraw === undefined) return;
        paintScheduled = true;
        requestAnimationFrame(() => requestAnimationFrame(() => mark('visibleFrame')));
    }
    let ready;
    Object.defineProperty(globalThis, '__BOKHEIM_STARTUP_READY__', {
        configurable: true,
        get() { return ready; },
        set(callback) {
            ready = (...args) => { mark('ready'); const result = callback(...args); maybePaint(); return result; };
        }
    });
    const observer = new MutationObserver(() => {
        const panel = document.getElementById('startup-status');
        const failure = panel?.querySelector('details pre');
        if (failure) { state.failure = failure.textContent; mark('failed'); }
        if (panel) state.sawPanel = true;
        if (state.sawPanel && !panel) { mark('panelRemoved'); maybePaint(); }
        if (document.querySelector('canvas')) mark('canvasCreated');
    });
    observer.observe(document, {childList: true, subtree: true});
    for (const type of ['WebGLRenderingContext', 'WebGL2RenderingContext']) {
        const proto = globalThis[type]?.prototype;
        if (!proto) continue;
        for (const name of ['drawArrays', 'drawElements', 'drawArraysInstanced', 'drawElementsInstanced']) {
            const original = proto[name];
            if (!original) continue;
            proto[name] = function(...args) {
                const result = original.apply(this, args);
                mark('firstDraw'); state.renderer = type; maybePaint(); return result;
            };
        }
    }
    if (globalThis.GPUQueue) {
        const original = GPUQueue.prototype.submit;
        GPUQueue.prototype.submit = function(...args) {
            const result = original.apply(this, args);
            mark('firstDraw'); state.renderer = 'WebGPU';
            if (!state.gpuCompletionRequested) {
                state.gpuCompletionRequested = true;
                this.onSubmittedWorkDone().then(() => {mark('firstGpuWorkDone'); maybePaint();});
            }
            maybePaint(); return result;
        };
    }
    const compile = WebAssembly.instantiateStreaming;
    WebAssembly.instantiateStreaming = async function(source, ...args) {
        const start = performance.now();
        const responseUrl = Promise.resolve(source).then(response => response?.url ?? '', () => '');
        const result = await compile.call(this, source, ...args);
        const end = performance.now();
        const url = await responseUrl;
        state.wasmModules.push({url, start, end});
        // Early startup also instantiates the coordinator on the Window.
        // Only the UI module belongs in the UI compilation milestone.
        if (/\/web_gpui_bg\.wasm(?:\?|$)/.test(url)) {
            mark('wasmInstantiateStart', start);
            mark('wasmInstantiateEnd', end);
        }
        return result;
    };
    for (const kind of ['Worker', 'SharedWorker']) {
        if (!globalThis[kind]) continue;
        globalThis[kind] = new Proxy(globalThis[kind], {construct(target, args, newTarget) {
            state.createdWorkers.push({kind, url:String(args[0]), name:args[1]?.name, ms:performance.now()});
            return Reflect.construct(target, args, newTarget);
        }});
    }
    addEventListener('error', e => state.errors.push(String(e.message)));
    addEventListener('unhandledrejection', e => state.errors.push(String(e.reason)));
})();

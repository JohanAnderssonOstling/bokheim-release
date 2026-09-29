import init, { initialize_pdfium_render, run_pdf_worker_job } from './web_gpui.js';
import createPdfium from './pdfium/pdfium.esm.js';

let ready;
self.onmessage = event => {
    if (event.data.module) {
        ready = (async () => {
            const pdfium = await createPdfium({ locateFile: file => new URL(`./pdfium/${file}`, import.meta.url).href });
            // Downloads can be retried before touching shared Rust/PDFium state.
            self.postMessage({ initializing: true });
            const rust = await init({ module_or_path: event.data.module, memory: event.data.memory });
            if (!initialize_pdfium_render(pdfium, rust, false)) throw new Error('PDFium WASM initialization failed');
        })();
        ready.catch(error => self.postMessage({ error: String(error) }));
    } else if (event.data.run) {
        ready.then(() => run_pdf_worker_job()).catch(error => self.postMessage({ error: String(error) }));
    }
};

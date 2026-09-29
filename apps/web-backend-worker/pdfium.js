// Each CPU worker owns its PDFium instance and its isolated Rust memory.
let ready;
export async function initialize_pdfium() {
    if (!ready) {
        ready = (async () => {
            const base = new URL('./', self.location.href);
            const {default: createPdfium} = await import(new URL('pdfium/pdfium.esm.js', base));
            const pdfium = await createPdfium({locateFile: file => new URL(`pdfium/${file}`, base).href});
            const {default: initialize, initialize_pdfium_render} = await import(new URL('cpu_worker.js', base));
            const rust = await initialize();
            if (!initialize_pdfium_render(pdfium, rust, false)) throw new Error('PDFium thumbnail initialization failed');
        })().catch(error => {
            ready = undefined;
            throw error;
        });
    }
    await ready;
}

// Speech stays in the browser; no model, audio download, or application TTS server.
let activeSpeech = null;

function environment() {
    const host = globalThis.window;
    try {
        const synth = host?.speechSynthesis;
        if (typeof host?.SpeechSynthesisUtterance !== "function" ||
            !["getVoices", "speak", "cancel", "pause", "resume"].every(name => typeof synth?.[name] === "function")) return null;
        return { synth, Utterance: host.SpeechSynthesisUtterance };
    } catch { return null; }
}

function voices() {
    try { return environment()?.synth.getVoices() ?? []; }
    catch { return []; }
}

export function available() { return voices().length > 0; }

function normalizeLanguage(language) {
    const normalized = language.trim().replaceAll("_", "-");
    try { return Intl.getCanonicalLocales(normalized)[0].toLowerCase(); }
    catch { return normalized.toLowerCase(); }
}

function baseLanguage(language) {
    return normalizeLanguage(language).split("-")[0];
}

function selectVoice(languages) {
    const choices = voices();
    const declared = languages.filter(language => language.trim());
    for (const language of declared) {
        const normalized = normalizeLanguage(language);
        const exact = choices.find(voice => normalizeLanguage(voice.lang) === normalized);
        if (exact) return exact;
        const match = choices.find(voice => baseLanguage(voice.lang) === baseLanguage(language));
        if (match) return match;
    }
    return declared.length ? null : choices.find(voice => voice.default) ?? choices[0] ?? null;
}

export function supportsLanguages(languages) { return selectVoice(JSON.parse(languages)) !== null; }

export class BrowserVoices {
    constructor(changed) {
        this.synth = environment()?.synth;
        this.changed = changed;
        this.synth?.addEventListener("voiceschanged", this.changed);
    }
    dispose() {
        this.synth?.removeEventListener("voiceschanged", this.changed);
        this.changed = null;
    }
}

export class BrowserSpeech {
    constructor(event) {
        if (!available()) throw new Error("Browser speech is unavailable.");
        Object.assign(this, environment());
        this.event = event;
        this.generation = 0;
        this.rate = 1;
        this.gap = 0;
        this.sentenceScale = 1;
        this.languages = [];
        this.end = Infinity;
        this.running = false;
        this.paused = false;
    }

    emit(kind, extra = {}) {
        this.event?.({ kind, generation: this.generation, ...extra });
    }

    invalidate(advanceGeneration = true) {
        if (advanceGeneration) this.generation += 1;
        clearTimeout(this.timer);
        this.timer = null;
        this.waiting = null;
        this.ready = null;
        if (this.utterance) {
            this.utterance.onstart = this.utterance.onend = this.utterance.onerror = null;
            this.utterance = null;
        }
        if (this.isCurrent()) this.synth.cancel();
        return this.generation;
    }

    start(count, index, glyph) {
        if (activeSpeech && activeSpeech !== this) {
            activeSpeech.emit("stopped");
            activeSpeech.stop();
        }
        activeSpeech = this;
        this.invalidate();
        this.count = count;
        this.index = index;
        this.glyph = glyph;
        this.end = Infinity;
        this.running = true;
        this.paused = false;
        this.opening = true;
        this.synth.resume();
        this.pump();
        return this.generation;
    }

    readItems(count, index, glyph) {
        this.passages = null;
        return this.start(count, index, glyph);
    }

    replacePassages(passages, count, index) {
        this.passages = JSON.parse(passages);
        this.part = this.passages.findIndex(chunk => chunk.index >= index);
        return this.start(count, index, 0);
    }

    pump() {
        if (!this.isCurrent() || !this.running || this.paused || this.utterance || this.waiting || this.timer !== null) return;
        if (this.index >= Math.min(this.count, this.end)) return this.finish();
        if (this.passages) {
            const chunk = this.passages[this.part];
            if (!chunk || chunk.index >= this.end) return this.finish();
            this.index = chunk.index;
            this.speak(chunk);
        } else {
            this.waiting = { index: this.index, glyph: this.glyph };
            this.emit("requested", { index: this.index, glyph_start: this.glyph, max_chars: this.opening ? 120 : 400 });
        }
    }

    provideChunk(generation, index, glyph, chunk) {
        if (!this.isCurrent() || generation !== this.generation || this.waiting?.index !== index || this.waiting.glyph !== glyph) return;
        this.waiting = null;
        const parsed = JSON.parse(chunk);
        if (!parsed) {
            this.index += 1;
            this.glyph = 0;
            this.pump();
            return;
        }
        this.ready = parsed;
        if (!this.paused) this.speak(parsed);
    }

    speak(chunk) {
        this.ready = null;
        if (this.index >= Math.min(this.count, this.end)) return this.finish();
        if (!chunk.text.trim()) { this.advance(chunk); return; }
        const voice = selectVoice(this.languages);
        if (!voice) return this.fail("No browser voice is available for this book’s language.");
        const generation = this.generation;
        const utterance = new this.Utterance(chunk.text);
        this.utterance = utterance; // Keep it alive until end/cancel.
        utterance.voice = voice;
        utterance.lang = voice.lang;
        utterance.rate = this.rate;
        const current = () => this.isCurrent() && this.generation === generation && this.utterance === utterance;
        utterance.onstart = () => {
            if (current()) this.emit("started", { index: this.index, glyph_ranges: chunk.glyph_ranges ?? [] });
        };
        utterance.onend = () => {
            if (!current()) return;
            this.utterance = null;
            this.advance(chunk);
        };
        utterance.onerror = event => {
            if (current()) this.fail(`Browser speech failed (${event.error || "unknown error"}). Try starting read aloud again.`);
        };
        try { this.synth.speak(utterance); }
        catch { this.fail("Browser speech could not start. Try starting read aloud again."); }
    }

    advance(chunk) {
        this.opening = false;
        if (this.passages) {
            this.part += 1;
            this.index = this.passages[this.part]?.index ?? this.count;
        } else if (chunk.next_glyph == null) {
            this.index += 1;
            this.glyph = 0;
        } else {
            this.glyph = chunk.next_glyph;
        }
        const delay = ((chunk.ends_block ? this.gap : 0) + Math.max(0, this.sentenceScale - 1) * 0.2) * 1000 / this.rate;
        if (delay > 0 && !this.paused) {
            this.timer = setTimeout(() => { this.timer = null; this.pump(); }, delay);
        } else this.pump();
    }

    finish() {
        this.running = false;
        this.emit("finished");
    }
    fail(message) {
        this.invalidate(false);
        this.running = false;
        this.emit("failed", { message });
    }
    isCurrent() { return activeSpeech === this; }
    cancel() {
        this.invalidate();
        this.running = false;
        return this.generation;
    }
    stop() {
        this.cancel();
        if (this.isCurrent()) activeSpeech = null;
    }
    skipTo(index) {
        if (!this.isCurrent()) return this.generation;
        this.invalidate();
        this.index = index;
        this.glyph = 0;
        if (this.passages) this.part = this.passages.findIndex(chunk => chunk.index >= index);
        this.running = true;
        this.opening = true;
        this.pump();
        return this.generation;
    }
    setEndFragment(end) {
        this.end = end ?? Infinity;
        return this.generation;
    }
    setRate(rate) {
        const next = Math.max(0.5, Math.min(3, rate));
        if (this.rate === next) return this.generation;
        this.rate = next;
        if (this.isCurrent() && this.running) this.restart();
        return this.generation;
    }
    setPauses(scale, gap) {
        this.sentenceScale = scale;
        this.gap = gap;
        return this.generation;
    }
    setLanguages(languages) { this.languages = JSON.parse(languages); }
    restart() {
        this.invalidate();
        this.pump();
    }
    pause() {
        if (!this.isCurrent()) return;
        this.paused = true;
        clearTimeout(this.timer);
        this.timer = null;
        this.synth.pause();
    }
    resume() {
        if (!this.isCurrent()) return;
        this.paused = false;
        this.synth.resume();
        if (this.ready) this.speak(this.ready);
        else this.pump();
    }
    dispose() {
        this.stop();
        this.event = null;
    }
}

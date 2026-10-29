import { test, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { available, supportsLanguages, BrowserVoices, BrowserSpeech } from './browser.js';

const endpoints = [];
afterEach(() => {
    for (const endpoint of endpoints.splice(0)) endpoint.dispose();
    delete globalThis.window;
});

class Synth extends EventTarget {
    voices = [{ name: 'English', lang: 'en-GB', default: true }, { name: 'Swedish', lang: 'sv-SE' }];
    utterances = [];
    paused = false;
    cancellations = 0;
    getVoices() { return this.voices; }
    speak(utterance) { this.utterances.push(utterance); }
    cancel() { this.cancellations++; }
    pause() { this.paused = true; }
    resume() { this.paused = false; }
}

function setup() {
    const synth = new Synth();
    globalThis.window = {
        speechSynthesis: synth,
        SpeechSynthesisUtterance: class { constructor(text) { this.text = text; } },
    };
    return synth;
}

function endpoint() {
    const events = [];
    const speech = new BrowserSpeech(event => events.push(event));
    endpoints.push(speech);
    return { speech, events };
}

function provide(speech, generation, index, glyph, text, next = null) {
    speech.provideChunk(generation, index, glyph, JSON.stringify({ text, glyph_ranges: [[glyph, glyph + text.length]], next_glyph: next, ends_block: next === null }));
}

test('hides speech without the API, an utterance constructor, or usable voices', () => {
    assert.equal(available(), false);
    const synth = setup();
    assert.equal(available(), true);
    synth.voices = [];
    assert.equal(available(), false);
    synth.voices = [{ lang: 'en' }];
    delete window.SpeechSynthesisUtterance;
    assert.equal(available(), false);
});

test('API presence alone and blocked voice enumeration do not enable speech', () => {
    const synth = setup();
    synth.getVoices = () => { throw Error('blocked'); };
    assert.equal(available(), false);
    synth.getVoices = () => [{ lang: 'en' }];
    synth.speak = undefined;
    assert.equal(available(), false);
});

test('late voices notify the reader and disposed listeners stop notifying', () => {
    const synth = setup();
    synth.voices = [];
    let changes = 0;
    const watch = new BrowserVoices(() => changes++);
    endpoints.push(watch);
    assert.equal(available(), false);
    synth.voices = [{ lang: 'en-GB' }];
    synth.dispatchEvent(new Event('voiceschanged'));
    assert.equal(changes, 1);
    assert.equal(available(), true);
    watch.dispose();
    synth.dispatchEvent(new Event('voiceschanged'));
    assert.equal(changes, 1);
});

test('matches book languages to installed voices and defaults when undeclared', () => {
    const synth = setup();
    assert.equal(supportsLanguages('[]'), true);
    assert.equal(supportsLanguages('["sv"]'), true);
    assert.equal(supportsLanguages('["swe"]'), true);
    assert.equal(supportsLanguages('["fr", "EN_us"]'), true);
    assert.equal(supportsLanguages('["fr"]'), false);
    const { speech } = endpoint();
    speech.setLanguages('["sv"]');
    const generation = speech.readItems(1, 0, 0);
    provide(speech, generation, 0, 0, 'Hej!');
    assert.equal(synth.utterances[0].lang, 'sv-SE');
});

test('canonicalizes three-letter book languages to browser voice language tags', () => {
    const synth = setup();
    synth.voices = [{ name: 'German', lang: 'de-DE' }];
    assert.equal(supportsLanguages('["deu"]'), true);
    assert.equal(supportsLanguages('["ger"]'), true);
    assert.equal(supportsLanguages('["fra"]'), false);
});

test('requests bounded EPUB chunks, highlights source ranges, and continues to completion', () => {
    const synth = setup();
    const { speech, events } = endpoint();
    const generation = speech.readItems(2, 0, 5);
    assert.deepEqual(events.at(-1), { kind: 'requested', generation, index: 0, glyph_start: 5, max_chars: 120 });
    provide(speech, generation, 0, 5, 'Hello.', 11);
    const first = synth.utterances.at(-1);
    first.onstart();
    assert.deepEqual(events.at(-1), { kind: 'started', generation, index: 0, glyph_ranges: [[5, 11]] });
    first.onend();
    assert.deepEqual(events.at(-1), { kind: 'requested', generation, index: 0, glyph_start: 11, max_chars: 400 });
    provide(speech, generation, 0, 11, 'Again.');
    synth.utterances.at(-1).onend();
    speech.provideChunk(generation, 1, 0, 'null');
    assert.deepEqual(events.at(-1), { kind: 'finished', generation });
});

test('skipping rejects stale chunks and completion callbacks', () => {
    const synth = setup();
    const { speech, events } = endpoint();
    const original = speech.readItems(3, 0, 0);
    provide(speech, original, 0, 0, 'First.');
    const lateEnd = synth.utterances[0].onend;
    const next = speech.skipTo(2);
    assert.ok(next > original);
    provide(speech, original, 0, 0, 'Stale.');
    lateEnd();
    assert.equal(synth.utterances.length, 1);
    assert.deepEqual(events.at(-1), { kind: 'requested', generation: next, index: 2, glyph_start: 0, max_chars: 120 });
    provide(speech, next, 2, 0, 'Third.');
    assert.equal(synth.utterances.at(-1).text, 'Third.');
});

test('pausing during text extraction retains the chunk until resume', () => {
    const synth = setup();
    const { speech } = endpoint();
    const generation = speech.readItems(1, 0, 0);
    speech.pause();
    provide(speech, generation, 0, 0, 'Ready.');
    assert.equal(synth.utterances.length, 0);
    speech.resume();
    assert.equal(synth.utterances.length, 1);
    assert.equal(synth.paused, false);
});

test('starting at a new location discards text received while paused', () => {
    const synth = setup();
    const { speech } = endpoint();
    const original = speech.readItems(2, 0, 0);
    speech.pause();
    provide(speech, original, 0, 0, 'Old location.');
    speech.readItems(2, 1, 0);
    speech.pause();
    speech.resume();
    assert.equal(synth.utterances.length, 0);
});

test('a sleep boundary set during extraction prevents the waiting chunk from speaking', () => {
    const synth = setup();
    const { speech, events } = endpoint();
    const generation = speech.readItems(2, 1, 0);
    speech.pause();
    provide(speech, generation, 1, 0, 'Next chapter.');
    speech.setEndFragment(1);
    speech.resume();
    assert.equal(synth.utterances.length, 0);
    assert.deepEqual(events.at(-1), { kind: 'finished', generation });
});

test('pausing a speaking utterance resumes it without replaying its start', () => {
    const synth = setup();
    const { speech } = endpoint();
    const generation = speech.readItems(1, 0, 0);
    provide(speech, generation, 0, 0, 'Still speaking.');
    speech.pause();
    assert.equal(synth.paused, true);
    speech.resume();
    assert.equal(synth.paused, false);
    assert.equal(synth.utterances.length, 1);
});

test('PDF passages continue across chunks and respect the chapter sleep boundary', () => {
    const synth = setup();
    const { speech, events } = endpoint();
    const generation = speech.replacePassages(JSON.stringify([
        { index: 0, text: 'First.' }, { index: 0, text: 'More.' }, { index: 1, text: 'Next chapter.' },
    ]), 2, 0);
    speech.setEndFragment(1);
    synth.utterances.at(-1).onend();
    assert.equal(synth.utterances.at(-1).text, 'More.');
    synth.utterances.at(-1).onend();
    assert.equal(synth.utterances.length, 2);
    assert.deepEqual(events.at(-1), { kind: 'finished', generation });
});

test('starting midway through a PDF page still allows skipping to an earlier passage', () => {
    const synth = setup();
    const { speech } = endpoint();
    speech.replacePassages('[{"index":0,"text":"Earlier."},{"index":1,"text":"Current."}]', 2, 1);
    assert.equal(synth.utterances.at(-1).text, 'Current.');
    speech.skipTo(0);
    assert.equal(synth.utterances.at(-1).text, 'Earlier.');
});

test('rate changes restart the current chunk and preserve a paused session', () => {
    const synth = setup();
    const { speech } = endpoint();
    const generation = speech.replacePassages('[{"index":0,"text":"First."}]', 1, 0);
    speech.pause();
    assert.ok(speech.setRate(1.5) > generation);
    assert.equal(synth.utterances.length, 1);
    speech.resume();
    assert.equal(synth.utterances.at(-1).rate, 1.5);
    assert.equal(synth.utterances.at(-1).text, 'First.');
});

test('another reader takes ownership and an old endpoint cannot stop it', () => {
    const synth = setup();
    const first = endpoint();
    const second = endpoint();
    const generation = first.speech.readItems(2, 0, 0);
    second.speech.replacePassages('[{"index":0,"text":"Second reader."}]', 1, 0);
    assert.deepEqual(first.events.at(-1), { kind: 'stopped', generation });
    assert.equal(first.speech.isCurrent(), false);
    const cancellations = synth.cancellations;
    first.speech.dispose();
    assert.equal(synth.cancellations, cancellations);
    assert.equal(second.speech.isCurrent(), true);
});

test('errors retain the session generation so the reader accepts the failure', () => {
    const synth = setup();
    const { speech, events } = endpoint();
    const generation = speech.readItems(2, 0, 0);
    provide(speech, generation, 0, 0, 'Hello.');
    synth.utterances.at(-1).onerror({ error: 'not-allowed' });
    assert.equal(events.at(-1).kind, 'failed');
    assert.equal(events.at(-1).generation, generation);
    assert.match(events.at(-1).message, /not-allowed/);
});

test('disposing cancels speech and ignores delayed text and utterance callbacks', () => {
    const synth = setup();
    const { speech, events } = endpoint();
    const generation = speech.readItems(2, 0, 0);
    provide(speech, generation, 0, 0, 'Hello.');
    const lateEnd = synth.utterances.at(-1).onend;
    speech.dispose();
    const length = events.length;
    lateEnd();
    provide(speech, generation, 1, 0, 'Late.');
    assert.equal(events.length, length);
    assert.equal(synth.utterances.length, 1);
    assert.equal(speech.isCurrent(), false);
});

test('pausing between passages does not enqueue the next one until resumed', () => {
    const synth = setup();
    const { speech } = endpoint();
    speech.replacePassages('[{"index":0,"text":"First."},{"index":1,"text":"Second."}]', 2, 0);
    speech.pause();
    synth.utterances[0].onend();
    assert.equal(synth.utterances.length, 1);
    speech.resume();
    assert.equal(synth.utterances.at(-1).text, 'Second.');
});

test('closing during a paragraph pause cancels its continuation', t => {
    t.mock.timers.enable({ apis: ['setTimeout'] });
    const synth = setup();
    const { speech, events } = endpoint();
    speech.setPauses(1, 0.4);
    speech.replacePassages('[{"index":0,"text":"First.","ends_block":true},{"index":1,"text":"Second."}]', 2, 0);
    synth.utterances[0].onend();
    speech.dispose();
    const length = events.length;
    t.mock.timers.tick(1000);
    assert.equal(events.length, length);
    assert.equal(synth.utterances.length, 1);
});

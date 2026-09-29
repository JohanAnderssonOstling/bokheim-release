import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import test from 'node:test';

class FakeAudio {
    constructor() {
        this.listeners = new Map();
        this.currentTime = 0;
        this.duration = 1;
        this.paused = true;
        this.ended = false;
        this.seeking = false;
        this.playbackRate = 1;
    }
    set src(value) { this._src = value; this.ended = false; this.currentTime = 0; }
    get src() { return this._src; }
    addEventListener(name, listener) {
        const listeners = this.listeners.get(name) || [];
        listeners.push(listener);
        this.listeners.set(name, listeners);
    }
    emit(name) { for (const listener of this.listeners.get(name) || []) listener(); }
    load() {}
    play() { this.paused = false; return Promise.resolve(); }
    pause() { this.paused = true; this.emit('pause'); }
    removeAttribute() { this.src = ''; }
}

globalThis.Audio = FakeAudio;
globalThis.addEventListener = () => {};
globalThis.removeEventListener = () => {};
const source = await readFile(new URL('../src/web_audio.js', import.meta.url), 'utf8');
const {BrowserAudio} = await import(`data:text/javascript;base64,${Buffer.from(source).toString('base64')}`);

test('a folder audiobook uses one global clock across track URLs', () => {
    const saved = [];
    const player = new BrowserAudio('https://example.test/play', [], {
        title: 'Book', tracks: [
            {start_ms: 0, end_ms: 1000},
            {start_ms: 1000, end_ms: 2000},
        ], bookDuration: 2,
    }, 1.25, (position, progress) => saved.push([position, progress]));
    assert.equal(player.audio.src, 'https://example.test/play/tracks/1');
    player.audio.emit('loadedmetadata');
    assert.equal(player.audio.currentTime, 0.25);
    assert.equal(player.position(), 1.25);

    player.seek(0.5);
    assert.equal(player.audio.src, 'https://example.test/play/tracks/0');
    player.audio.emit('loadedmetadata');
    assert.equal(player.audio.currentTime, 0.5);
    assert.equal(player.position(), 0.5);

    player.play();
    player.audio.currentTime = 1;
    player.audio.ended = true;
    player.audio.paused = true;
    player.audio.emit('pause');
    player.audio.emit('ended');
    assert.equal(player.audio.src, 'https://example.test/play/tracks/1');
    player.audio.emit('loadedmetadata');
    assert.equal(player.position(), 1);
    assert.equal(player.playing(), true);
    assert.ok(saved.some(([position, progress]) => position === 0.5 && progress === 0.25));
    player.dispose();
});

test('downloaded folder switches between local track blobs', () => {
    const player = new BrowserAudio('', [], {
        title: 'Offline book', tracks: [
            {start_ms: 0, end_ms: 1000},
            {start_ms: 1000, end_ms: 2000},
        ], localTrackUrls: ['blob:first', 'blob:second'], bookDuration: 2,
    }, 0, () => {});
    assert.equal(player.audio.src, 'blob:first');
    player.audio.emit('loadedmetadata');
    player.seek(1.5);
    assert.equal(player.audio.src, 'blob:second');
    player.audio.emit('loadedmetadata');
    assert.equal(player.position(), 1.5);
    player.dispose();
});

import {test} from 'node:test';
import assert from 'node:assert/strict';
import {BrowserAudio} from './web_audio.js';

class AudioStub extends EventTarget {
    currentTime = 0;
    duration = 120;
    paused = true;
    ended = false;
    playbackRate = 1;
    defaultPlaybackRate = 1;
    play() { this.paused = false; this.dispatchEvent(new Event('playing')); return Promise.resolve(); }
    pause() { this.paused = true; this.dispatchEvent(new Event('pause')); }
    removeAttribute() {}
    load() { this.playbackRate = this.defaultPlaybackRate; }
}

function directPlayer(t, renew) {
    const {player: old} = setup(); old.dispose();
    const released = [];
    t.mock.method(globalThis, 'fetch', async (url, options) => { released.push([url, options.method]); return {}; });
    const config = {url: 'https://audio.invalid/api/playback/old', expires_in_seconds: 900, checksum: 'revision'};
    const player = new BrowserAudio(config.url, new Uint8Array(), {title: 'Direct'}, 60, () => {}, config, renew);
    player.audio.dispatchEvent(new Event('loadedmetadata'));
    player.validUntil = 0;
    return {player, released, config};
}

test('expired direct media renews at the confirmed position and releases the previous grant', async t => {
    const {player, released, config} = directPlayer(t, async () => ({...config, url: 'https://audio.invalid/api/playback/new'}));
    player.setRate(1.5);
    player.play();
    await new Promise(resolve => setTimeout(resolve, 550));
    assert.equal(player.url, 'https://audio.invalid/api/playback/new');
    player.audio.dispatchEvent(new Event('loadedmetadata'));
    assert.equal(player.position(), 60);
    assert.equal(player.audio.paused, false);
    assert.equal(player.audio.crossOrigin, 'anonymous');
    assert.equal(player.rate(), 1.5);
    assert.deepEqual(released, [[config.url, 'DELETE']]);
    player.dispose();
    assert.equal(released.length, 2);
});

test('pause discards a late direct grant without restarting audio', async t => {
    let finish;
    const pending = new Promise(resolve => { finish = resolve; });
    const {player, released, config} = directPlayer(t, () => pending);
    player.play();
    await new Promise(resolve => setTimeout(resolve, 550));
    player.pause();
    finish({...config, url: 'https://audio.invalid/api/playback/late'});
    await new Promise(resolve => setTimeout(resolve, 0));
    assert.equal(player.audio.paused, true);
    assert.equal(player.url, config.url);
    assert.deepEqual(released, [['https://audio.invalid/api/playback/late', 'DELETE']]);
    player.dispose();
});

test('close cancels direct retry backoff without issuing another grant', async t => {
    let calls = 0;
    const {player} = directPlayer(t, async () => { calls++; throw Error('offline'); });
    player.play(); player.dispose();
    await new Promise(resolve => setTimeout(resolve, 550));
    assert.equal(calls, 0);
    assert.equal(player.audio.paused, true);
});

test('media error followed by play rejection retains retry intent', async t => {
    const {player, config} = directPlayer(t, async () => ({...config, url: 'https://audio.invalid/api/playback/new'}));
    player.validUntil = Infinity;
    player.audio.play = () => {
        player.audio.error = {code: 4};
        player.audio.dispatchEvent(new Event('error'));
        return Promise.reject(Object.assign(Error('source failed'), {name: 'NotSupportedError'}));
    };
    player.play();
    await Promise.resolve();
    assert.equal(player.wantsPlay, true);
    assert.equal(player.buffering(), true);
    assert.equal(player.takeError(), null);
    player.audio.play = AudioStub.prototype.play;
    await new Promise(resolve => setTimeout(resolve, 550));
    assert.equal(player.audio.paused, false);
    player.dispose();
});

test('page exit revokes the grant and a retained page renews only on explicit Play', async t => {
    const {player, released, config} = directPlayer(t, async () => ({...config, url: 'https://audio.invalid/api/playback/new'}));
    player.validUntil = Infinity;
    player.play();
    player.visibilitychange();
    assert.equal(released.length, 0, 'background playback must not revoke');
    player.pagehide();
    assert.equal(player.playing(), false);
    assert.equal(player.validUntil, 0);
    assert.deepEqual(released, [[config.url, 'DELETE']]);
    player.play();
    await new Promise(resolve => setTimeout(resolve, 550));
    assert.equal(player.url, 'https://audio.invalid/api/playback/new');
    assert.equal(player.audio.paused, false);
    player.dispose();
});

test('a rejected direct play starts recovery even before the media error event', async t => {
    const {player, config} = directPlayer(t, async () => ({...config, url: 'https://audio.invalid/api/playback/new'}));
    player.validUntil = Infinity;
    player.audio.play = () => Promise.reject(Object.assign(Error('source failed'), {name: 'NotSupportedError'}));
    player.play();
    await Promise.resolve();
    assert.equal(player.wantsPlay, true);
    assert.equal(player.buffering(), true);
    player.dispose();
});

test('a late play rejection cannot restart playback after Pause', async t => {
    const {player} = directPlayer(t, async () => { throw Error('must not renew'); });
    player.validUntil = Infinity;
    let reject;
    player.audio.play = () => new Promise((_, fail) => { reject = fail; });
    player.play();
    player.pause();
    reject(Object.assign(Error('source failed'), {name: 'NotSupportedError'}));
    await Promise.resolve();
    assert.equal(player.wantsPlay, false);
    assert.equal(player.recovery, null);
    assert.equal(player.takeError(), null);
    player.dispose();
});
function setup(media = true) {
    const actions = new Map();
    const session = {setActionHandler: (name, callback) => actions.set(name, callback), setPositionState: state => {session.position = state;}};
    globalThis.Audio = AudioStub;
    globalThis.MediaMetadata = class { constructor(data) {Object.assign(this, data);} };
    Object.defineProperty(globalThis, 'navigator', {value: {mediaSession: media ? session : undefined}, configurable: true});
    globalThis.addEventListener = () => {};
    globalThis.removeEventListener = () => {};
    const saves = [];
    const player = new BrowserAudio("blob:test-audiobook", new Uint8Array(), {title: 'Test book', artist: 'Author'}, 60, (...args) => saves.push(args));
    player.audio.dispatchEvent(new Event('loadedmetadata'));
    return {player, actions, session, saves};
}

test('media errors under direct recovery do not discard the session', async t => {
    const {player, config} = directPlayer(t, async () => ({...config, url: 'https://audio.invalid/api/playback/new'}));
    player.validUntil = Infinity;
    player.play();
    player.audio.error = {code: 2};
    player.audio.dispatchEvent(new Event('error'));
    assert.equal(player.failed(), false);
    assert.equal(player.buffering(), true);
    player.pause();
    assert.equal(player.failed(), true);
    player.dispose();
});

test('renewal rejects and revokes a changed revision without playing it', async t => {
    const {player, released, config} = directPlayer(t, async () => ({...config, checksum: 'changed', url: 'https://audio.invalid/api/playback/changed'}));
    player.play();
    await new Promise(resolve => setTimeout(resolve, 550));
    assert.equal(player.audio.paused, true);
    assert.equal(player.url, config.url);
    assert.match(player.takeError(), /revision changed/);
    assert.deepEqual(released, [['https://audio.invalid/api/playback/changed', 'DELETE']]);
    player.dispose();
});

test('lock-screen actions use 15/30 seconds, clamp bounds, and persist without a UI timer', () => {
    const {player, actions, session, saves} = setup();
    actions.get('seekbackward')({});
    assert.equal(player.position(), 45);
    actions.get('seekforward')({});
    assert.equal(player.position(), 75);
    actions.get('seekto')({seekTime: 200});
    assert.equal(player.position(), 120);
    actions.get('seekto')({seekTime: 4});
    actions.get('seekbackward')({});
    assert.equal(player.position(), 0);
    assert.equal(saves.at(-1)[0], 0);
    actions.get('play')();
    assert.equal(session.playbackState, 'playing');
    actions.get('pause')();
    assert.equal(session.playbackState, 'paused');
    player.setRate(1.5);
    assert.equal(session.position.playbackRate, 1.5);
    player.dispose();
    assert.equal(actions.get('play'), null);
});

test('a replaced reader cannot clear the new session or save after disposal', () => {
    const first = setup();
    const second = new BrowserAudio("blob:test-audiobook", new Uint8Array(), {title: 'Second'}, 0, () => {});
    const count = first.saves.length;
    first.player.audio.dispatchEvent(new Event('seeked'));
    first.player.dispose();
    assert.equal(first.saves.length, count);
    assert.equal(first.session.metadata.title, 'Second');
    second.dispose();
});

test('autoplay rejection permits a later user gesture and browsers without Media Session still play', async () => {
    const {player} = setup(false);
    player.audio.play = () => Promise.reject(Object.assign(new Error('blocked'), {name: 'NotAllowedError'}));
    player.play();
    await Promise.resolve();
    assert.match(player.takeError(), /Tap Play/);
    player.audio.play = AudioStub.prototype.play;
    player.toggle();
    assert.equal(player.playing(), true);
    player.dispose();
});

test('initial position is not persisted before metadata confirms it', () => {
    const {player, saves} = setup();
    const nextSaves = [];
    const next = new BrowserAudio("blob:test-audiobook", new Uint8Array(), {}, 85, (...args) => nextSaves.push(args));
    next.audio.dispatchEvent(new Event('timeupdate'));
    assert.equal(nextSaves.length, 0);
    next.audio.dispatchEvent(new Event('loadedmetadata'));
    assert.equal(nextSaves[0][0], 85);
    next.dispose();
});

test('pending seek cannot become a saved resume position', () => {
    const {player, saves} = setup();
    const count = saves.length;
    player.audio.seeking = true;
    player.seek(95);
    player.audio.dispatchEvent(new Event('timeupdate'));
    player.audio.dispatchEvent(new Event('pause'));
    assert.equal(player.confirmedPosition(), undefined);
    assert.equal(player.lastConfirmedPosition(), 60);
    assert.equal(saves.length, count);
    player.dispose();
    assert.equal(saves.length, count);
    assert.equal(player.lastConfirmedPosition(), 60);
});

test('only a completed seek updates progress, including while paused', () => {
    const {player, saves} = setup();
    player.audio.seeking = true;
    player.seek(95);
    player.audio.dispatchEvent(new Event('seeked'));
    assert.equal(player.confirmedPosition(), undefined, 'stale seeked while still seeking');
    player.audio.seeking = false;
    player.audio.dispatchEvent(new Event('seeked'));
    assert.equal(player.confirmedPosition(), 95);
    assert.equal(saves.at(-1)[0], 95);
    player.dispose();
});

test('buffering holds position and pause suppresses its transport state', () => {
    const {player} = setup();
    player.play();
    player.audio.dispatchEvent(new Event('waiting'));
    assert.equal(player.buffering(), true);
    player.audio.currentTime = 63;
    assert.equal(player.position(), 60);
    assert.equal(player.confirmedPosition(), 60);
    player.toggle();
    assert.equal(player.buffering(), false);
    assert.equal(player.playing(), false);
    assert.equal(player.confirmedPosition(), 60);
    player.audio.dispatchEvent(new Event('canplay'));
    assert.equal(player.playing(), false, 'data arrival must not undo pause');
    player.dispose();
});

test('initial asynchronous seek is not confirmed by metadata alone', () => {
    setup();
    const saves = [];
    const player = new BrowserAudio('blob:test', new Uint8Array(), {}, 85, (...args) => saves.push(args));
    player.audio.seeking = true;
    player.audio.dispatchEvent(new Event('loadedmetadata'));
    assert.equal(player.confirmedPosition(), undefined);
    assert.equal(saves.length, 0);
    player.audio.seeking = false;
    player.audio.dispatchEvent(new Event('seeked'));
    assert.equal(saves[0][0], 85);
    player.dispose();
});

test('explicit pause while buffering is idempotent', () => {
    const {player} = setup();
    player.play();
    player.audio.dispatchEvent(new Event('waiting'));
    player.pause();
    player.pause();
    assert.equal(player.playing(), false);
    assert.equal(player.buffering(), false);
    player.audio.dispatchEvent(new Event('canplay'));
    assert.equal(player.playing(), false);
    player.dispose();
    player.pause();
    assert.equal(player.playing(), false);
});

test('terminal media failure remains detectable after the UI consumes its message', () => {
    const {player} = setup();
    player.audio.seeking = true;
    player.seek(90);
    player.audio.error = {code: 2};
    player.audio.dispatchEvent(new Event('error'));
    assert.equal(player.failed(), true);
    assert.match(player.takeError(), /media error 2/);
    assert.equal(player.takeError(), null);
    assert.equal(player.failed(), true);
    assert.equal(player.lastConfirmedPosition(), 60, 'failed seek must not become reopen position');
    player.dispose();
});

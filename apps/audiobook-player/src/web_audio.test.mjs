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
    const config = {url: 'https://audio.invalid/api/playback/old', expires_in_seconds: 900, content_hash: 'revision'};
    const player = new BrowserAudio(config.url, new Uint8Array(), {title: 'Direct'}, 60, () => {}, config, renew);
    player.audio.dispatchEvent(new Event('loadedmetadata'));
    player.validUntil = 0;
    return {player, released, config};
}


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

test('range streaming follows media-session pause, page exit and disposal without requesting a direct grant', t => {
    const {player: previous, actions} = setup();
    previous.dispose();
    t.mock.method(globalThis, 'fetch', () => { throw Error('range playback must not request a direct playback grant'); });
    const paused = [];
    const player = new BrowserAudio('/.bokheim-audio/source', new Uint8Array(), {title: 'Drive book'}, 0, () => {}, null, null, value => paused.push(value));
    assert.equal(paused.at(-1), true);
    player.audio.dispatchEvent(new Event('loadedmetadata'));
    actions.get('play')();
    assert.equal(paused.at(-1), false);
    actions.get('pause')();
    assert.equal(paused.at(-1), true);
    actions.get('play')();
    player.pagehide();
    assert.equal(player.playing(), false);
    assert.equal(paused.at(-1), true);
    player.play();
    player.dispose();
    assert.equal(paused.at(-1), true);
});

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
    const {player, released, config} = directPlayer(t, async () => ({...config, content_hash: 'changed', url: 'https://audio.invalid/api/playback/changed'}));
    player.play();
    await new Promise(resolve => setTimeout(resolve, 550));
    assert.equal(player.audio.paused, true);
    assert.equal(player.url, config.url);
    assert.match(player.takeError(), /revision changed/);
    assert.deepEqual(released, [['https://audio.invalid/api/playback/changed', 'DELETE']]);
    player.dispose();
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

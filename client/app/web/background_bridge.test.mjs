import test from 'node:test';
import assert from 'node:assert/strict';
import { policy } from './policy_wasm.mjs';
let service;
const listeners=[];
globalThis.self={
    performance:globalThis.performance, setTimeout, clearTimeout,
    addEventListener(_,listener){listeners.push(listener);},
    postMessage(message,transfer){
        const received=structuredClone(message,{transfer});
        service=policy.backgroundService(received.port,received.options.retry,received.options.debounce);
    },
};
globalThis.addEventListener=self.addEventListener;
globalThis.postMessage=self.postMessage;

test('scheduler bridge delivers cycles, reports completion, and stops both sides',async()=>{
    const scheduler=policy.backgroundHost(1000,2);
    try {
        scheduler.notify('refresh');
        assert.equal(await scheduler.next(),true);
        scheduler.notify('wake');
        scheduler.finish(true);
        assert.equal(await scheduler.next(),false);
        scheduler.finish(true);
        const pending=scheduler.next();
        const rejected=assert.rejects(pending,/stopped/);
        scheduler.stop();
        await rejected;
        await new Promise(resolve=>setTimeout(resolve,10));
        assert.equal(service.stopped,true);
    }finally{scheduler.stop();service.stop();}
});

test('coordinator shutdown rejects a waiting scheduler without leaving timers',async()=>{
    const scheduler=policy.backgroundHost(1000,2);
    const rejected=assert.rejects(scheduler.next(),/stopped/);
    for(const listener of listeners)listener({data:{transport:'sync_failed',error:'worker failed'}});
    await rejected;
    await new Promise(resolve=>setTimeout(resolve,10));
    assert.equal(service.stopped,true);
});


test('stopping discards a cycle already buffered on the host',async()=>{
    const scheduler=policy.backgroundHost(1000,2);
    scheduler.notify('refresh');
    await new Promise(resolve=>setTimeout(resolve,20));
    scheduler.stop();
    scheduler.stop();
    await assert.rejects(scheduler.next(),/stopped/);
    await new Promise(resolve=>setTimeout(resolve,10));
    assert.equal(service.stopped,true);
});

test('a deferred retry crosses the bridge without polling while idle',async()=>{
    const scheduler=policy.backgroundHost(10,2);
    try {
        let received=false;
        const pending=scheduler.next().then(value=>{received=true;return value;});
        await new Promise(resolve=>setTimeout(resolve,40));
        assert.equal(received,false);
        scheduler.wake_after(20);
        assert.equal(await pending,false);
        scheduler.finish(true);
    }finally{scheduler.stop();service.stop();}
});

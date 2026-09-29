import test from 'node:test';
import assert from 'node:assert/strict';
import { policy } from './policy_wasm.mjs';

function setup() {
    let now=0, stopped=false;
    const sent=[];
    const timing=policy.backgroundTiming(0,30,5);
    function message(data) {
        if(stopped)return;
        if(data.kind==='wake')timing.wake(now);
        if(data.kind==='refresh')timing.refresh(now);
        if(data.kind==='done')timing.finish(now,data.ok);
        if(data.kind==='stop'){stopped=true;timing.free();}
    }
    function advance(target) {
        if(!stopped && timing.deadline()<=target) {
            now=timing.deadline();
            const remote=timing.start(now);
            if(remote!==undefined)sent.push({kind:'cycle',remote});
        }
        now=target;
    }
    return {sent,advance,message};
}

test('login and server notifications start immediately', () => {
    const s=setup(); s.message({kind:'refresh'}); s.advance(0);
    assert.deepEqual(s.sent,[{kind:'cycle',remote:true}]);
});

test('continuous writes share a fixed debounce deadline and cannot overlap a pass', () => {
    const s=setup();
    s.message({kind:'wake'});
    s.advance(3); s.message({kind:'wake'});
    s.advance(5); assert.deepEqual(s.sent,[{kind:'cycle',remote:false}]);
    s.message({kind:'wake'}); s.advance(100);
    assert.equal(s.sent.length,1);
    s.message({kind:'done',ok:true}); s.advance(100);
    assert.equal(s.sent.length,2,'writes during the pass get one follow-up');
});

test('failed remote work retries after backoff despite write notifications', () => {
    const s=setup(); s.message({kind:'refresh'}); s.advance(5);
    assert.equal(s.sent[0].remote,true);
    s.message({kind:'done',ok:false});
    for (let time=6;time<35;time++) {s.advance(time);s.message({kind:'wake'});}
    assert.equal(s.sent.length,1);
    s.advance(35); assert.equal(s.sent.length,2); assert.equal(s.sent[1].remote,true);
});

test('explicit refresh during a pass runs immediately after completion', () => {
    const s=setup(); s.message({kind:'refresh'}); s.advance(5);
    s.advance(6); s.message({kind:'refresh'}); s.message({kind:'done',ok:true});
    s.advance(6); assert.equal(s.sent.length,2); assert.equal(s.sent[1].remote,true);
});

test('idle and successful work schedule no periodic checks', () => {
    const s=setup(); s.advance(1000); assert.equal(s.sent.length,0);
    s.message({kind:'wake'}); s.advance(1005); assert.equal(s.sent.length,1);
    s.message({kind:'done',ok:true}); s.advance(2000); assert.equal(s.sent.length,1);
    s.message({kind:'stop'}); s.advance(3000); assert.equal(s.sent.length,1);
});

test('failed local work retains a retry deadline without another wakeup', () => {
    const s=setup(); s.message({kind:'wake'}); s.advance(5);
    s.message({kind:'done',ok:false}); s.advance(34); assert.equal(s.sent.length,1);
    s.advance(35); assert.equal(s.sent.length,2); assert.equal(s.sent[1].remote,false);
    s.message({kind:'done',ok:true}); s.advance(1000); assert.equal(s.sent.length,2);
});

import test from 'node:test';
import assert from 'node:assert/strict';
import { policy } from './policy_wasm.mjs';
globalThis.self=globalThis;
globalThis.addEventListener=()=>{};
function setup() {
    const workers=[];
    let closed=false;
    const coordinator = policy.startCoordinator((url)=>{
        const worker={url,messages:[],terminated:false,postMessage(message){this.messages.push(message);},terminate(){this.terminated=true;}};
        workers.push(worker); return worker;
    },()=>{closed=true;});
    return {workers,connect:port=>coordinator.connect(port),get closed(){return closed;},database:workers[0]};
}
// Keep the native MessagePort identity: production decoding rejects plain objects.
function port() {
    const {port1, port2} = new MessageChannel();
    port2.close();
    port1.sent = [];
    port1.postMessage = function(message) { this.sent.push(message); };
    const close = port1.close.bind(port1);
    port1.close = function() { this.closed = true; close(); };
    return port1;
}
function crash(worker,message) { worker.onerror({preventDefault(){},message}); }

test('fatal coordinator failure terminates the database and rejects waiting and later clients',()=>{
    const s=setup(); const waiting=port(); s.connect(waiting);
    assert.equal(s.database.messages.length,0);
    crash(s.database,'storage worker crashed');
    assert.equal(s.database.terminated,true); assert.equal(s.closed,true);
    assert.equal(waiting.closed,true); assert.match(waiting.sent[0].error,/crashed/);
    const sent=s.database.messages.length;
    crash(s.database,'duplicate failure');
    assert.equal(s.database.messages.length,sent);
    s.database.onmessage({data:{transport:'backend_ready'}});
    const late=port(); s.connect(late);
    assert.equal(late.closed,true); assert.match(late.sent[0].error,/crashed/);
});

test('readiness connects waiting clients and fatal failure closes their relays',()=>{
    const s=setup(); const client=port(); s.connect(client);
    s.database.onmessage({data:{transport:'backend_ready'}});
    assert.equal(s.database.messages[0].transport,'connect');
    // The fake worker does not receive ownership of this transferred port.
    s.database.messages[0].port.close();
    crash(s.database,'shutdown');
    assert.equal(client.closed,true); assert.match(client.sent[0].error,/shutdown/);
});

test('HTTP failure preserves the database and permits another request',async()=>{
    const originalFetch=globalThis.fetch;
    let calls=0;
    globalThis.fetch=async()=>{ calls++; throw new Error('HTTP unavailable'); };
    const s=setup();
    try {
        const request=()=>{
            const reply=port();
            s.database.onmessage({data:{transport:'network_request',port:reply,request:{url:'https://example.invalid',options:{}}}});
            return reply;
        };
        const first=request();
        await new Promise(resolve=>setTimeout(resolve,0));
        assert.match(first.sent[0].error,/HTTP unavailable/);
        assert.equal(first.closed,true);
        assert.equal(s.database.terminated,false);
        assert.equal(s.closed,false);
        const second=request();
        await new Promise(resolve=>setTimeout(resolve,0));
        assert.match(second.sent[0].error,/HTTP unavailable/);
        assert.equal(calls,2);
        assert.equal(s.workers.length,1,'HTTP runs in the coordinator');
        crash(s.database,'shutdown');
        const late=request();
        assert.equal(late.sent.length,0);
        assert.equal(calls,2,'late requests cannot start HTTP after shutdown');
    } finally { globalThis.fetch=originalFetch; }
});

test('three background services run independently and all stop on failure', async () => {
    const s=setup();
    const ports=Array.from({length:3},()=>port());
    for (const p of ports) {
        s.database.onmessage({data:{transport:'background_start',port:p,options:{retry:1000,debounce:1}}});
    }
    try {
        for (const p of ports) p.onmessage({data:{kind:'refresh'}});
        await new Promise(resolve=>setTimeout(resolve,30));
        for (const p of ports) {
            assert.equal(p.closed,undefined);
            assert.equal(p.sent[0]?.kind,'cycle');
        }
        // Leave account and inactive-outbox cycles outstanding.
        const active=ports[0];
        active.onmessage({data:{kind:'refresh'}});
        active.onmessage({data:{kind:'cycle_result',id:active.sent[0].id,success:true}});
        await new Promise(resolve=>setTimeout(resolve,30));
        assert.equal(active.sent.filter(m=>m.kind==='cycle').length,2);
        assert.equal(ports[1].sent.length,1);
        assert.equal(ports[2].sent.length,1);
    } finally {
        crash(s.database,'shutdown');
        for (const p of ports) assert.equal(p.closed,true);
    }
});

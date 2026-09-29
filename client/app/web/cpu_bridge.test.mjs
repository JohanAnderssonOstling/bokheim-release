import test from 'node:test';
import assert from 'node:assert/strict';
import {policy} from './policy_wasm.mjs';
const requests=[];
globalThis.postMessage=(message,transfer)=>requests.push(structuredClone(message,{transfer}));

test('CPU reply ports match out-of-order completions and isolate duplicate replies',async()=>{
    const first=policy.cpuRequest(new Uint8Array([1]));
    const second=policy.cpuRequest(new Uint8Array([2]));
    const [a,b]=requests.splice(0);
    b.port.postMessage({bytes:new Uint8Array([20])});
    assert.deepEqual(await second,new Uint8Array([20]));
    b.port.postMessage({bytes:new Uint8Array([99])});
    const rejected=assert.rejects(first,/worker failed/);
    a.port.postMessage({error:'worker failed'});
    await rejected;
    a.port.close();b.port.close();
});

test('CPU dispatch failures reject without installing a global listener',async()=>{
    const send=globalThis.postMessage;
    globalThis.postMessage=()=>{throw Error('coordinator unavailable');};
    try {
        assert.throws(()=>policy.cpuRequest(new Uint8Array([1])),/coordinator unavailable/);
    }finally{globalThis.postMessage=send;}
});

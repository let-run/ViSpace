import test from 'node:test';
import assert from 'node:assert/strict';
import {blobEncode,blobDecode,crc32,SyncNetwork} from './sync-client.js';
test('blob framing matches native CRC32 and rejects corrupted or truncated files',()=>{const data=new TextEncoder().encode('123456789');assert.equal(crc32(data),0xcbf43926);const wire=blobEncode('asset:782e706466',data);assert.equal(new TextDecoder().decode(wire.slice(0,8)),'BOMBLB01');assert.deepEqual(blobDecode(wire),{asset:'asset:782e706466',bytes:data});wire[wire.length-1]^=1;assert.throws(()=>blobDecode(wire),/checksum/);assert.throws(()=>blobDecode(wire.slice(0,10)),/Invalid/);});
const local='000000000000000000000001',remote='000000000000000000000002';
class Socket {constructor(url){this.url=url;this.readyState=1;this.bufferedAmount=0;this.messages=[];Socket.latest=this;}send(message){this.messages.push(message);}close(){this.readyState=3;}}
function fixture(){let queue=Promise.resolve(),received=[],blobs=[],view={clock:{},sharedHead:{},publish:[]};const a={identity:()=>local,view:()=>view,enqueue:fn=>{const task=queue.then(fn);queue=task.catch(()=>{});return task;},notify(){},notePeer:async()=>{},encode:r=>new TextEncoder().encode(r.id),decode:()=>({id:'incoming'}),assets:()=>['asset:78'],currentAssets:()=>[],getBlob:async()=>new Uint8Array([1,2]),putBlob:async(...v)=>blobs.push(v),receive:async r=>received.push(r)};const network=new SyncNetwork(a,Socket);
 // These framing tests start after identity authentication; real cryptographic pairing is covered by browser/WSS integration.
 a.auth={start(p){p.authenticated=false;network.authenticated(p);},update(){return false;},save(){},i:{ledger:()=> '[]',verify_record(){},merge:()=>false}};
 network.connect('wss://localhost');const socket=Socket.latest;const peer=network.peers.get('1');peer.authenticated=true;peer.authPeer={node_id:remote};peer.liveLedger='[]';const tick=async()=>{for(let i=0;i<15;i++)await new Promise(r=>setTimeout(r,0));};return{network,socket,view,tick,received,blobs};}
test('pull completes before local publication, attachments precede events, and repeats are suppressed',async()=>{const f=fixture();f.view.publish=[{id:'outgoing',mesh:{origin:local,sequence:1}}];f.socket.onopen();await f.tick();f.socket.onmessage({data:JSON.stringify({type:'hello',version:9,application_id:remote,clock:{}})});await f.tick();assert.equal(f.socket.messages.length,1);assert.equal(f.network.pulling(),true);f.socket.onmessage({data:JSON.stringify({type:'ready',version:9,clock:{}})});await f.tick();assert.equal(f.network.pulling(),false);assert.deepEqual(blobDecode(f.socket.messages[1]).bytes,new Uint8Array([1,2]));assert.equal(new TextDecoder().decode(f.socket.messages[2]),'outgoing');assert.equal(JSON.parse(f.socket.messages[3]).type,'ready');f.network.changed();await f.tick();assert.equal(f.socket.messages.length,4);});
test('received blobs are saved before event acknowledgement; invalid identities release pull hold',async()=>{const f=fixture();f.socket.onmessage({data:JSON.stringify({type:'hello',version:9,application_id:remote,clock:{}})});await f.tick();f.socket.onmessage({data:blobEncode('asset:78',new Uint8Array([4])).buffer});f.socket.onmessage({data:new Uint8Array([1]).buffer});await f.tick();assert.equal(f.blobs.length,1);assert.deepEqual(f.received,[{id:'incoming'}]);assert.equal(JSON.parse(f.socket.messages[0]).type,'ack');const g=fixture();g.socket.onmessage({data:JSON.stringify({type:'hello',version:9,application_id:local,clock:{}})});await g.tick();assert.equal(g.network.pulling(),false);assert.match(g.network.statuses()[0].status,/same application/);});
test('topology travels before inventory publication and repeated versions do not circulate',async()=>{const f=fixture();let ads=[{id:local,title:'Here',epoch:1,sequence:1,neighbors:[]}],unlinked=0,received=0;f.network.a.topologyAds=()=>ads;f.network.a.topologyConnect=()=>{};f.network.a.topologyDisconnect=()=>unlinked++;f.network.a.topologyReceive=()=>received++;f.socket.onopen();await f.tick();assert.deepEqual(JSON.parse(f.socket.messages[0]).topology,ads);f.socket.onmessage({data:JSON.stringify({type:'hello',version:9,application_id:remote,clock:{},topology:[]})});await f.tick();assert.equal(received,1);assert.equal(JSON.parse(f.socket.messages[1]).type,'topology');const count=f.socket.messages.length;f.network.changed();await f.tick();assert.equal(f.socket.messages.length,count);ads=[{...ads[0],sequence:2,neighbors:[remote]}];f.socket.onmessage({data:JSON.stringify({type:'topology',advertisements:ads})});await f.tick();assert.equal(received,2);assert.equal(f.socket.messages.length,count+1);assert.equal(JSON.parse(f.socket.messages.at(-1)).advertisements[0].sequence,2);f.network.disconnect('1');f.socket.onclose();assert.equal(unlinked,1);});

test("base addresses select the sync endpoint",()=>{const f=fixture();assert.equal(f.socket.url,"wss://localhost/sync");assert.equal(f.network.statuses()[0].url,"wss://localhost");assert.throws(()=>f.network.connect("wss://localhost/raft"),/Raft sync has been removed/);});

test('a browser reports an old Raft peer',async()=>{const f=fixture();f.socket.onmessage({data:JSON.stringify({type:'hello',version:6,application_id:remote,clock:{},cluster:remote})});await f.tick();assert.match(f.network.statuses()[0].status,/Raft sync has been removed/);});

test('a protocol mismatch remains visible after close and stops automatic reconnect',async()=>{
 const f=fixture();
 f.socket.onmessage({data:JSON.stringify({type:'hello',version:8,application_id:remote,clock:{}})});
 f.socket.onclose();
 await f.tick();
 assert.match(f.network.statuses()[0].status,/browser 9, desktop 8/);
 assert.equal(f.network.canReconnect('wss://localhost/sync'),false);
 f.network.connect('wss://localhost');
 assert.equal(f.network.canReconnect('wss://localhost'),true);
});

test('a desktop rejection received before hello survives socket closure',async()=>{
 const f=fixture();
 const message='Branches have diverged. Choose Reconnect branches in Sync.';
 f.socket.onmessage({data:JSON.stringify({type:'error',message})});
 f.socket.onclose();
 await f.tick();
 assert.equal(f.network.statuses()[0].status,message);
 assert.equal(f.network.canReconnect('wss://localhost'),false);
});

test('bundle availability wakes waiting transport without disconnecting sync',async()=>{
 const f=fixture();let wakes=0;f.network.a.bundleReady=()=>wakes++;
 f.socket.onmessage({data:JSON.stringify({type:'hello',version:9,application_id:remote,clock:{}})});await f.tick();
 f.socket.onmessage({data:JSON.stringify({type:'ready',version:9,clock:{}})});await f.tick();
 f.socket.onmessage({data:JSON.stringify({type:'git_bundle_ready'})});await f.tick();
 assert.equal(wakes,1);assert.equal(f.network.statuses()[0].status,'Live');
});

test('a live peer can recover its bundle capability without opening an inbound browser listener',async()=>{
 const grant='00000000-0000-4000-8000-000000000002';
 const f=fixture();f.socket.onmessage({data:JSON.stringify({type:'hello',version:9,application_id:remote,clock:{}})});await f.tick();
 f.socket.onmessage({data:JSON.stringify({type:'ready',version:9,clock:{}})});await f.tick();
 assert.equal(f.network.statuses()[0].status,'Live');assert.equal(f.network.bundlePeers().length,0);
 f.socket.onmessage({data:JSON.stringify({type:'git_bundle_ready',git_bundle_grant:grant})});await f.tick();
 assert.equal(f.network.bundlePeers().length,1);assert.equal(f.network.bundlePeers()[0].grant,grant);assert.equal(f.network.statuses()[0].gitBundles,true);
 f.network.disconnect('1');assert.equal(f.network.bundlePeers().length,0);
});

test('offline saves can distinguish an open socket from a pending connection',()=>{
 const f=fixture();
 f.socket.readyState=0;
 assert.equal(f.network.connected(),false);
 f.socket.readyState=1;
 assert.equal(f.network.connected(),true);
 f.network.disconnect('1');
 assert.equal(f.network.connected(),false);
});

test('an open but unauthenticated socket has no network access',()=>{const f=fixture();const p=f.network.peers.get('1');p.authenticated=false;assert.equal(f.network.connected(),false);assert.equal(f.network.bundlePeers().length,0);f.network.disconnect('1');});

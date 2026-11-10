// Approved P-256 peers authenticate before hello; files precede events and clients pull before publishing.
const encoder=new TextEncoder(),decoder=new TextDecoder('utf-8',{fatal:true});
const MAGIC=encoder.encode('BOMBLB01'),MAX=64*1024*1024;
export function crc32(bytes){let crc=0xffffffff;for(const b of bytes){crc^=b;for(let i=0;i<8;i++)crc=(crc>>>1)^((crc&1)?0xedb88320:0);}return(crc^0xffffffff)>>>0;}
export function blobEncode(asset,bytes){if(bytes.length>MAX)throw Error('Attachment exceeds 64 MiB');const header=encoder.encode(JSON.stringify({asset,size:bytes.length,crc32:crc32(bytes)}));if(header.length>4096)throw Error('Attachment path is too long');const result=new Uint8Array(12+header.length+bytes.length);result.set(MAGIC);new DataView(result.buffer).setUint32(8,header.length,true);result.set(header,12);result.set(bytes,12+header.length);return result;}
export function isBlob(bytes){return bytes.length>=8&&MAGIC.every((b,i)=>bytes[i]===b);}
export function blobDecode(bytes){if(bytes.length<12||!isBlob(bytes))throw Error('Invalid attachment message');const length=new DataView(bytes.buffer,bytes.byteOffset,bytes.byteLength).getUint32(8,true);if(length>4096||length+12>bytes.length)throw Error('Invalid attachment header');const header=JSON.parse(decoder.decode(bytes.subarray(12,12+length))),data=bytes.subarray(12+length);if(!Number.isSafeInteger(header.size)||header.size>MAX||data.length!==header.size||crc32(data)!==header.crc32)throw Error('Invalid attachment size or checksum');return{asset:header.asset,bytes:data};}
export const nodeId=/^[0-9a-f]{24}$/;
const syncVersion=9;
const uuid=/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
function clock(value){if(!value||typeof value!=='object'||Array.isArray(value)||Object.entries(value).some(([id,n])=>!(nodeId.test(id)||uuid.test(id))||!Number.isSafeInteger(n)||n<0))throw Error('Invalid peer clock');return value;}
export class SyncNetwork {
 constructor(adapter,Socket=globalThis.WebSocket){this.a=adapter;this.Socket=Socket;this.peers=new Map();this.serial=0;this.heartbeat=setInterval(()=>{if(this.a.topologyTick?.()||this.a.component?.())this.changed();},20000);this.heartbeat.unref?.();}
 connected(){return [...this.peers.values()].some(peer=>peer.active&&peer.authenticated&&peer.socket.readyState===1);}
 pulling(){return [...this.peers.values()].some(p=>p.active&&p.authenticated&&!p.ready);}
 canReconnect(url){const origin=new URL(url).origin;return ![...this.peers.values()].some(p=>p.url===origin&&p.retryBlocked);}
 bundlePeers(){return [...this.peers.values()].filter(p=>p.active&&p.ready&&p.gitBundleGrant).map(p=>({url:p.url,grant:p.gitBundleGrant}));}
 statuses(){return [...this.peers.values()].map(({id,url,status,peerId,head,active,ready,gitBundleGrant,authPeer,authProven})=>({id,url,status,peerId,pairingId:authProven?authPeer.node_id:undefined,head,active,ready,gitBundles:!!gitBundleGrant}));}
 authenticated(p){if(p.authenticated)return;p.authenticated=true;clearTimeout(p.authDeadline);const v=this.a.view();this.send(p,{type:'hello',version:syncVersion,application_id:this.a.identity(),git_bundle_notifications:true,clock:v.clock,component:!!this.a.component?.(),browser:true,head:this.a.component?.()?v.head:v.sharedHead,topology:this.a.topologyAds?.()??[]});}
 changed(){this.a.notify();for(const p of this.peers.values()){try{if(this.a.auth&&p.active&&p.socket.readyState===1&&this.a.auth.update(p,m=>this.send(p,m))&&!p.authenticated)this.authenticated(p);if(p.authenticated&&p.hello){const actions=this.a.auth.i.ledger();if(p.liveLedger!==actions){this.send(p,{type:'membership',actions:JSON.parse(actions)});p.liveLedger=actions;}}this.pump(p);}catch(e){this.fail(p,e);}}}
 connect(url,expectedId=null,approveRemote=false){const parsed=new URL(url);if(expectedId!==null&&!nodeId.test(expectedId))throw Error('Invalid discovered node ID');if(['/raft','/raft/'].includes(parsed.pathname))throw Error('Raft sync has been removed. Use the WebSocket base address in Sync (/sync).');if(this.a.component?.()&&[...this.peers.values()].some(p=>p.active))throw Error('Disconnect the current browser gateway before connecting another one');if(!['wss:'].includes(parsed.protocol)||parsed.username||parsed.password||parsed.search||parsed.hash||!['/','/sync'].includes(parsed.pathname))throw Error('Enter a WebSocket base address, such as wss://localhost:8824');if([...this.peers.values()].filter(p=>p.active).length>=32)throw Error('Too many connections');
  for(const [id,peer]of this.peers)if(!peer.active&&peer.url===parsed.origin)this.peers.delete(id);
  parsed.pathname='/sync';const socket=new this.Socket(parsed.href),p={id:String(++this.serial),url:parsed.origin,expectedId,approveRemote,expectedHostname:parsed.hostname.endsWith('.vispace.local')?parsed.hostname:null,socket,status:'Connecting',active:true,ready:false,hello:false,sent:new Set(),assets:new Set(),clock:{},scheduled:false,topology:new Map()};this.peers.set(p.id,p);socket.binaryType='arraybuffer';
  const run=fn=>this.a.enqueue(fn).catch(e=>this.fail(p,e));
  socket.onopen=()=>run(async()=>{if(!p.active)return;this.a.auth.start(p,m=>this.send(p,m));});
  socket.onmessage=event=>{if(typeof event.data!=='string'&&event.data.byteLength>MAX+16384)return this.fail(p,Error('Sync message exceeds size limit'));if(typeof event.data==='string'&&event.data.length>MAX)return this.fail(p,Error('Sync control exceeds size limit'));if(typeof event.data==='string'&&p.proposal){
    let message;try{message=JSON.parse(event.data);}catch{}
    if(message?.type==='save_result'&&message.id===p.proposal.id){p.proposal.finish({...message.outcome,record:message.record});return;}
   }run(()=>this.receive(p,event.data));};
  // Let queued messages, including a final peer error, finish before reporting the close.
  socket.onerror=()=>run(()=>this.fail(p,Error('Connection failed')));
  socket.onclose=event=>run(()=>{if(p.active){p.active=false;p.status=event?.reason||'Disconnected';this.unlink(p);this.changed();}});this.changed();return p.id;
 }
 disconnect(id){const p=this.peers.get(id);if(p){p.active=false;p.status='Disconnected';this.unlink(p);p.socket.close();this.changed();}}
 unlink(p){clearTimeout(p.authDeadline);p.proposal?.finish({result:'unavailable'});if(p.topologyConnected){p.topologyConnected=false;this.a.topologyDisconnect?.(p.peerId);}}
 fail(p,error){if(!p.active)return;p.active=false;clearTimeout(p.authDeadline);this.unlink(p);p.status=String(error.message||error);try{p.socket.close(1000);}catch{}this.changed();}
 send(p,value){if(!p.active||p.socket.readyState!==1)throw Error('Connection closed');p.socket.send(typeof value==='object'&&!(value instanceof Uint8Array)?JSON.stringify(value):value);}
 async drain(p){const start=Date.now();while(p.socket.bufferedAmount>1024*1024){if(!p.active||Date.now()-start>30000)throw Error('Sync send timed out');await new Promise(r=>setTimeout(r,10));}}
 async receive(p,data){if(!p.active)return;
  if(!p.authenticated){if(typeof data!=='string')throw Error('Authentication required');const done=this.a.auth.receive(p,JSON.parse(data),m=>this.send(p,m));if(done)this.authenticated(p);this.changed();return;}
  if(typeof data==='string'){const m=JSON.parse(data);
   if(m.type==='error'){p.retryBlocked=true;throw Error(m.message);}
   if(m.type==='hello'){
    p.retryBlocked=true;
    if(m.cluster!=null)throw Error('Raft sync has been removed. Upgrade this desktop to use Partition sync.');
    if(m.version!==syncVersion)throw Error(`Sync protocol mismatch: browser ${syncVersion}, desktop ${m.version}. Update both apps to the same release.`);
    if(p.hello||!nodeId.test(m.application_id)||m.application_id===this.a.identity()||m.application_id!==p.authPeer.node_id)throw Error('Incompatible peer or same application identity');
    if(m.component){
     if([...this.peers.values()].some(other=>other!==p&&other.active))throw Error('Partition sync uses one browser gateway. Reconnect branches from a desktop.');
     this.a.joinComponent?.();p.component=true;
    }else if(this.a.component?.())throw Error('This desktop does not use partition sync');
    p.hello=true;p.gitBundleGrant=uuid.test(m.git_bundle_grant ?? "")?m.git_bundle_grant:null;p.peerId=m.application_id;p.head=m.head;p.clock=clock(m.clock);p.status='Receiving changes';this.a.topologyReceive?.(m.topology??[]);this.a.topologyConnect?.(p.peerId);p.topologyConnected=true;await this.a.notePeer(p.peerId,p.clock);p.retryBlocked=false;}
   else {if(!p.hello)throw Error('Peer did not introduce itself');if(m.type==='ready'){if(m.version!==syncVersion)throw Error('Incompatible sync version');p.clock=clock(m.clock);p.ready=true;p.status='Live';await this.a.notePeer(p.peerId,p.clock);}else if(m.type==='authority'&&p.component){if(m.leader!==null&&!nodeId.test(m.leader))throw Error('Invalid authority status');p.leader=m.leader;p.authoritySeen=Date.now();}else if(m.type==='save_result'&&p.component){}else if(m.type==='ack'){p.clock=clock(m.clock);await this.a.notePeer(p.peerId,p.clock);}else if(m.type==='membership'){if(this.a.auth.i.merge(JSON.stringify(m.actions)))this.a.auth.save();}else if(m.type==='git_bundle_ready'){if(uuid.test(m.git_bundle_grant??''))p.gitBundleGrant=m.git_bundle_grant;this.a.bundleReady?.();}else if(m.type==='topology'){this.a.topologyReceive?.(m.advertisements);}else throw Error('Unknown sync message');}
  } else {if(!p.hello)throw Error('Peer did not introduce itself');const bytes=new Uint8Array(data);if(isBlob(bytes)){const b=blobDecode(bytes);await this.a.putBlob(b.asset,b.bytes);}else{const record=this.a.decode(bytes);this.a.auth.i.verify_record(JSON.stringify(record));await this.a.receive(record);await this.a.notePeer(p.peerId,this.a.view().clock);this.send(p,{type:'ack',id:record.id,applied:true,clock:this.a.view().clock});}}
  this.changed();
 }
 leader(){return [...this.peers.values()].find(p=>p.active&&p.ready&&p.component&&p.leader&&Date.now()-p.authoritySeen<45000)?.leader??null;}
 async propose(record){
  const peer=[...this.peers.values()].find(p=>p.active&&p.ready&&p.component&&p.leader&&Date.now()-p.authoritySeen<45000);
  if(!peer)return {result:'unavailable'};
  for(const asset of this.a.assets(record)){
   if(peer.assets.has(asset))continue;
   this.send(peer,blobEncode(asset,await this.a.getBlob(asset)));await this.drain(peer);peer.assets.add(asset);
  }
  return new Promise((resolve,reject)=>{
   const finish=outcome=>{clearTimeout(timer);peer.proposal=null;resolve(outcome);};
   const timer=setTimeout(()=>finish({result:'unavailable'}),15000);
   peer.proposal={id:record.id,finish};
   try{this.send(peer,this.a.encode(record,true));}
   catch(error){clearTimeout(timer);peer.proposal=null;reject(error);}
  });
 }
 pump(p){if(!p.active||!p.hello||p.scheduled)return;p.scheduled=true;
  // One event per turn lets incoming files, local saves and comparisons make progress.
  this.a.enqueue(async()=>{if(!p.active)return;
   const ads=(this.a.topologyAds?.()??[]).filter(ad=>p.topology.get(ad.id)!==`${ad.epoch}:${ad.sequence}`);
   if(ads.length){for(const ad of ads)p.topology.set(ad.id,`${ad.epoch}:${ad.sequence}`);this.send(p,{type:'topology',advertisements:ads});await this.drain(p);return true;}
   if(!p.ready)return false;
   const v=this.a.view(),record=!p.component&&v.publish.find(r=>!p.sent.has(r.id)&&(p.clock[r.mesh.origin]||0)<r.mesh.sequence);
   if(record){for(const asset of this.a.assets(record)){if(!p.assets.has(asset)){const bytes=await this.a.getBlob(asset);this.send(p,blobEncode(asset,bytes));await this.drain(p);p.assets.add(asset);}}this.send(p,this.a.encode(record));p.sent.add(record.id);await this.drain(p);}
   else if(!p.sentReady){ // Supply files that the other side may have lost since an earlier connection.
    for(const asset of p.component?[]:this.a.currentAssets()){if(p.assets.has(asset))continue;const bytes=await this.a.getBlob(asset,true);if(bytes){this.send(p,blobEncode(asset,bytes));await this.drain(p);p.assets.add(asset);}}
    this.send(p,{type:'ready',version:syncVersion,clock:v.clock});p.sentReady=true;
   }
   return !!record;
  }).then(more=>{p.scheduled=false;if(more)this.pump(p);},e=>{p.scheduled=false;this.fail(p,e);});
 }
}

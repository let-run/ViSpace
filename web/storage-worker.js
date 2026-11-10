import {requireWorkspacePermission as permission,workspaceWritable} from './workspace-access.js';
// The worker owns SQLite and all durable inventory writes. The UI never receives a save ack early.
import {NetworkAuth} from './network-auth.js';
import {SyncNetwork,blobEncode,blobDecode} from './sync-client.js';
import sqliteInit from './sqlite/sqlite3.mjs';
import rustInit,{BrowserIdentity,browser_apply_git_snapshot,browser_history_chunk,browser_branch_merge,browser_proposal_reviews,browser_rebase_proposal,BrowserTopology,browser_storage_project_references,browser_storage_local,browser_storage_make,browser_storage_replay,browser_storage_frame,browser_storage_decode,browser_sync_inspect,browser_sync_resolve,browser_sync_encode,browser_sync_decode,browser_sync_assets} from './pkg/vispace.js';

let sql,db,pool,privateRoot,workspace=null,revision=0,history=[],snapshot=null,activeChunk=null;
let security,auth;
let viewCache=null,hydratedBase,hydratedText,topology;
const empty=()=>({parts:[],purchases:[],projects:[],storage:{},allocations:[],bom:[]});
const encode=new TextEncoder(),decode=new TextDecoder();
// Preserve imported attachment paths when their hydrated UI values are unchanged.
const hydratedAssets=new Map(),hydratedByAsset=new Map();
function pathParts(path){const parts=path.split('/');if(parts.some(p=>!p||p==='.'||p==='..'||p.includes(String.fromCharCode(92))||p.includes(':')||p.includes(String.fromCharCode(0))))throw Error('Unsafe workspace path');return parts;}

async function directory(root,parts,create=false){for(const part of parts)root=await root.getDirectoryHandle(part,{create});return root;}
async function read(root,path){const parts=pathParts(path),name=parts.pop();const dir=await directory(root,parts);const handle=await dir.getFileHandle(name);const file=await handle.getFile();return new Uint8Array(await file.arrayBuffer());}

async function exists(root,path){try{return await read(root,path);}catch(e){if(e.name==='NotFoundError')return null;throw e;}}
async function write(root,path,bytes){const parts=pathParts(path),name=parts.pop(),dir=await directory(root,parts,true);const handle=await dir.getFileHandle(name,{create:true});const writer=await handle.createWritable();try{await writer.write(bytes);await writer.close();}catch(e){await writer.abort().catch(()=>{});throw e;}}
async function immutable(root,path,bytes){const old=await exists(root,path);if(old){if(old.length!==bytes.length||!old.every((b,i)=>b===bytes[i]))throw Error(`Different files use the same path: ${path}`);return;}await write(root,path,bytes);}

async function rootsWrite(path,bytes,immutableFile=false){await permission(workspace);for(const root of workspace?[workspace,privateRoot]:[privateRoot])await(immutableFile?immutable:write)(root,path,bytes);}
function asset(path){return 'asset:'+Array.from(encode.encode(path),b=>b.toString(16).padStart(2,'0')).join('');}
function assetPath(id){if(!/^asset:(?:[0-9a-f]{2})+$/i.test(id))throw Error('Invalid asset reference');const path=decode.decode(Uint8Array.from(id.slice(6).match(/../g),b=>parseInt(b,16)));pathParts(path);return 'Attachments/'+path;}
function base64(bytes){let s='';for(let i=0;i<bytes.length;i+=32768)s+=String.fromCharCode(...bytes.subarray(i,i+32768));return btoa(s);}
async function externalize(value,prior){if(typeof value==='string'){
 const match=/^data:(image\/[^;,]+|application\/pdf)(;base64)?,(.*)$/s.exec(value);if(match){if(typeof prior==='string'&&prior.startsWith('asset:')&&hydratedByAsset.get(prior)===value)return prior;if(hydratedAssets.has(value))return hydratedAssets.get(value);const bytes=match[2]?Uint8Array.from(atob(match[3]),c=>c.charCodeAt(0)):encode.encode(decodeURIComponent(match[3]));if(bytes.length>64*1024*1024)throw Error('Attachment exceeds 64 MiB');const hash=Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256',bytes)),b=>b.toString(16).padStart(2,'0')).join('');const extension=({'image/png':'png','image/jpeg':'jpg','image/webp':'webp','image/gif':'gif','image/svg+xml':'svg','image/tiff':'tiff','application/pdf':'pdf'})[match[1]]||'bin';const path=`browser/${hash}.${extension}`;await rootsWrite('Attachments/'+path,bytes,true);const id=asset(path);hydratedAssets.set(value,id);hydratedByAsset.set(id,value);return id;}
 return value.startsWith('asset:')||value.startsWith('literal:')?'literal:'+value:value;
 }if(Array.isArray(value)){const key=v=>v?.id??v?.part?.id;return Promise.all(value.map((v,i)=>externalize(v,Array.isArray(prior)?(key(v)?prior.find(p=>key(p)===key(v)):prior[i]):undefined)));}if(value&&typeof value==='object'){const result={};for(const[k,v]of Object.entries(value))result[k]=await externalize(v,prior?.[k]);return result;}return value;}
async function hydrate(value){if(typeof value==='string'){if(value.startsWith('literal:'))return value.slice(8);if(value.startsWith('asset:')){const path=assetPath(value);let bytes=await exists(privateRoot,path);if(!bytes&&workspace){bytes=await exists(workspace,path);if(bytes)await immutable(privateRoot,path,bytes);}if(!bytes)throw Error(`Missing attachment: ${path}`);const ext=path.split('.').pop().toLowerCase(),mime=({png:'image/png',jpg:'image/jpeg',jpeg:'image/jpeg',webp:'image/webp',gif:'image/gif',svg:'image/svg+xml',tiff:'image/tiff',pdf:'application/pdf'})[ext]||'application/octet-stream';const uri=`data:${mime};base64,${base64(bytes)}`;hydratedAssets.set(uri,value);hydratedByAsset.set(value,uri);return uri;}return value;}if(Array.isArray(value))return Promise.all(value.map(hydrate));if(value&&typeof value==='object'){const result={};for(const[k,v]of Object.entries(value))result[k]=await hydrate(v);return result;}return value;}
async function chunks(root){let dir;try{dir=await root.getDirectoryHandle('inventory.actions');}catch(e){if(e.name==='NotFoundError')return[];throw e;}const files=[];for await(const[name,handle]of dir.entries())if(handle.kind==='file'&&name.endsWith('.wal'))files.push([name,handle]);files.sort(([a],[b])=>a.localeCompare(b));const records=[];for(const[,handle]of files){const file=await handle.getFile();if(file.size>72*1024*1024)throw Error('Action chunk exceeds size limit');records.push(...JSON.parse(browser_storage_decode(new Uint8Array(await file.arrayBuffer()))));}return records;}
async function historyPage(before, limit) {
 if (!Number.isInteger(limit) || limit < 1 || limit > 200) throw Error('Invalid history page size');
 const entries = [];
 let dir;
 try {
  dir = await privateRoot.getDirectoryHandle('inventory.actions');
 } catch (error) {
  if (error.name === 'NotFoundError') return {entries, next: null};
  throw error;
 }
 const names = [];
 for await (const [name, handle] of dir.entries()) {
  if (handle.kind === 'file' && name.endsWith('.wal') && (!before || name <= before.chunk)) names.push(name);
 }
 for (const name of names.sort().reverse()) {
  const file = await (await dir.getFileHandle(name)).getFile();
  if (file.size > 72 * 1024 * 1024) throw Error('Action chunk exceeds size limit');
  const offset = before?.chunk === name ? before.offset : undefined;
  const [page, more] = JSON.parse(browser_history_chunk(
   new Uint8Array(await file.arrayBuffer()), name, offset, limit - entries.length
  ));
  entries.push(...page);
  if (more) {
   const {chunk, offset} = entries.at(-1).entry;
   return {entries, next: {chunk, offset}};
  }
 }
 return {entries, next: null};
}
function union(records){const byId=new Map(history.map(r=>[r.id,r]));for(const r of records){const old=byId.get(r.id);if(old){if(!r.mesh)continue;if(JSON.stringify(old)!==JSON.stringify(r)){
 // Replaying both makes the shared Rust engine validate IDs and contents canonically.
 browser_storage_replay(JSON.stringify([old,r]));}continue;}byId.set(r.id,r);}return [...byId.values()];}
async function append(record){const frame=browser_storage_frame(JSON.stringify(record));if(!activeChunk||activeChunk.size+frame.length>8*1024*1024){activeChunk={name:`${String(Date.now()).padStart(20,'0')}-${crypto.randomUUID().toUpperCase()}.wal`,size:8};await rootsWrite('inventory.actions/'+activeChunk.name,encode.encode('BOMLOG01'));}
 await permission(workspace);for(const root of workspace?[workspace,privateRoot]:[privateRoot]){const dir=await directory(root,['inventory.actions'],true),file=await dir.getFileHandle(activeChunk.name);const writer=await file.createWritable({keepExistingData:true});try{await writer.seek(activeChunk.size);await writer.write(frame);await writer.close();}catch(e){await writer.abort().catch(()=>{});activeChunk=null;throw e;}}
 activeChunk.size+=frame.length;
}
function exec(sql,bind=[]){db.exec({sql,bind});}
const componentOrigin=crypto.randomUUID().toUpperCase(),componentQueue=[];
function component(){return !!db.selectValue('SELECT 1 FROM sync_authority WHERE singleton=1');}
function joinComponent(){
 if(component())return;
 exec("INSERT INTO sync_authority VALUES(1,'component')");
}
function pending(){return componentQueue[0]??null;}
function savePending(value){
 const prior=componentQueue[0];
 if(value)componentQueue[0]=value;else componentQueue.shift();
 if(value?.legacy)exec('INSERT OR REPLACE INTO sync_pending VALUES(1,?)',[JSON.stringify(value)]);
 else if(prior?.legacy)exec('DELETE FROM sync_pending');
}
function identity(){return db.selectValue('SELECT application_id FROM sync_identity WHERE singleton=1');}
function inspect(records=history){const pulling=network.pulling();if(records===history&&viewCache?.records===records&&viewCache.pulling===pulling)return viewCache.value;const value=JSON.parse(browser_sync_inspect(JSON.stringify(records),identity(),pulling));if(records===history)viewCache={records,pulling,value};return value;}
function project(records,local=snapshot){const result={...inspect(records)};if(result.reviews.length)result.inventory=snapshot||empty();else if(local)result.inventory=JSON.parse(browser_storage_local(JSON.stringify(result.inventory),JSON.stringify(local)));return result;}

let appliedHead = '';
function commit(result){const data=result.inventory;
 const stored=browser_storage_project_references(JSON.stringify(data),true),rows=JSON.parse(stored);
 const nextHead = result.reviews.length ? appliedHead : result.appliedTip;exec('BEGIN IMMEDIATE');try{
 for(const table of ['project_kicad','project_git_tips','project_git_remote','project_notes','allocations','parts','projects','project_ids','purchases','project_bom_entries','storage_descriptions','sync_events'])exec(`DELETE FROM ${table}`);
 for(const p of data.projects){exec('INSERT INTO projects VALUES(?,?)',[p.name,p.description]);exec('INSERT INTO project_ids VALUES(?,?)',[p.name,p.id]);if(p.git_tips)exec('INSERT INTO project_git_tips VALUES(?,?)',[p.name,JSON.stringify(p.git_tips)]);if(p.git_remote)exec('INSERT INTO project_git_remote VALUES(?,?)',[p.name,JSON.stringify(p.git_remote)]);if(p.kicad)exec('INSERT INTO project_kicad VALUES(?,?)',[p.name,JSON.stringify(p.kicad)]);exec('INSERT INTO project_notes VALUES(?,?,?)',[p.name,p.notesAttachmentID,JSON.stringify(p.notes)]);}
 for(const p of data.parts)exec('INSERT INTO parts VALUES(?,?,?)',[p.id,p.barcode,JSON.stringify(p)]);
 for(const a of rows.allocations)exec('INSERT INTO allocations VALUES(?,?,?)',[a.id,a.partID,JSON.stringify(a)]);
 for(const p of rows.purchases)exec('INSERT INTO purchases VALUES(?,?)',[p.part.id,JSON.stringify(p)]);
 for(const b of rows.bom)exec('INSERT INTO project_bom_entries VALUES(?,?)',[b.id,JSON.stringify(b)]);
 for(const[name,description]of Object.entries(data.storage))exec('INSERT INTO storage_descriptions VALUES(?,?)',[name,description]);
 for(const r of result.events){const m=r.mesh;exec('INSERT INTO sync_events VALUES(?,?,?,?,?,?)',[r.id,m.origin,m.sequence,m.lamport,r.timestamp_ms,JSON.stringify(r)]);}
 const next=revision+1;exec('INSERT INTO browser_state VALUES(1,?,?) ON CONFLICT(id) DO UPDATE SET snapshot=excluded.snapshot,revision=excluded.revision',[stored,next]);exec('INSERT INTO inventory_applied_head VALUES(1,?) ON CONFLICT(singleton) DO UPDATE SET hash=excluded.hash', [nextHead]);
 if(network.connected())exec('INSERT OR REPLACE INTO sync_connection_head VALUES(1,?)',[nextHead]);
 exec('COMMIT');appliedHead=nextHead;revision=next;snapshot=data;history=result.events;
 }catch(e){exec('ROLLBACK');throw e;}}
async function recover(){let records=await chunks(privateRoot);if(!component()&&workspace&&await workspaceWritable(workspace))records.push(...await chunks(workspace));const ids=new Set(records.map(r=>r.id));const previous=workspace;try{if(workspace&&!await workspaceWritable(workspace))workspace=null;for(const record of history)if(!ids.has(record.id))await append(record);}finally{workspace=previous;}const merged=union(records);if(merged.length){const result=project(merged);commit(result);} }
async function loadDemoInventory() {
 const response = await fetch(new URL('./demo/inventory.json', import.meta.url));
 if (!response.ok) throw Error(`Could not load the demo inventory (HTTP ${response.status}).`);
 return response.text();
}
async function init({legacy,folder,demo}){
 await rustInit();sql=await sqliteInit({locateFile:file=>new URL('./sqlite/'+file,import.meta.url).href});
 if(!navigator.storage?.getDirectory)throw Error('This browser needs persistent OPFS storage to open SQLite.');
 // A single writer owns the database; never silently open a second, independent copy.
 let acquired;const lockReady=new Promise(r=>acquired=r);navigator.locks.request('bom-inventory.sqlite',{ifAvailable:true},lock=>{acquired(!!lock);return lock?new Promise(()=>{}):undefined;});if(!await lockReady)throw Error('Inventory is already open in another tab. Close it before continuing.');
 pool=await sql.installOpfsSAHPoolVfs({name:'inventory-sahpool',directory:'.bom-inventory-sqlite',initialCapacity:8,preserveOnInitFailure:true});db=new pool.OpfsSAHPoolDb('/inventory.sqlite','c');
 db.exec(`PRAGMA foreign_keys=ON; CREATE TABLE IF NOT EXISTS browser_state(id INTEGER PRIMARY KEY,snapshot TEXT,revision INTEGER);CREATE TABLE IF NOT EXISTS sync_identity(singleton INTEGER PRIMARY KEY,application_id TEXT NOT NULL);CREATE TABLE IF NOT EXISTS sync_events(id TEXT PRIMARY KEY,origin TEXT,sequence INTEGER,lamport INTEGER,timestamp_ms INTEGER,payload TEXT,UNIQUE(origin,sequence));CREATE TABLE IF NOT EXISTS sync_peers(id TEXT PRIMARY KEY,last_received_ms INTEGER,clock TEXT);CREATE TABLE IF NOT EXISTS sync_merge_drafts(id TEXT PRIMARY KEY,text TEXT);CREATE TABLE IF NOT EXISTS projects(name TEXT PRIMARY KEY COLLATE NOCASE,description TEXT NOT NULL);CREATE TABLE IF NOT EXISTS project_ids(name TEXT PRIMARY KEY COLLATE NOCASE,id TEXT UNIQUE);CREATE TABLE IF NOT EXISTS project_git_tips(project TEXT PRIMARY KEY COLLATE NOCASE,payload TEXT NOT NULL);CREATE TABLE IF NOT EXISTS project_git_remote(project TEXT PRIMARY KEY COLLATE NOCASE,payload TEXT NOT NULL);CREATE TABLE IF NOT EXISTS project_kicad(project TEXT PRIMARY KEY COLLATE NOCASE,payload TEXT);CREATE TABLE IF NOT EXISTS project_notes(project TEXT PRIMARY KEY COLLATE NOCASE,attachment_id TEXT,payload TEXT);CREATE TABLE IF NOT EXISTS parts(id TEXT PRIMARY KEY,barcode TEXT UNIQUE,payload TEXT);CREATE TABLE IF NOT EXISTS allocations(id TEXT PRIMARY KEY,part_id TEXT REFERENCES parts(id),payload TEXT);CREATE TABLE IF NOT EXISTS purchases(id TEXT PRIMARY KEY,payload TEXT);CREATE TABLE IF NOT EXISTS project_bom_entries(id TEXT PRIMARY KEY,payload TEXT);CREATE TABLE IF NOT EXISTS storage_descriptions(name TEXT PRIMARY KEY COLLATE NOCASE,description TEXT);`);
 db.exec('CREATE TABLE IF NOT EXISTS sync_authority(singleton INTEGER PRIMARY KEY,cluster TEXT NOT NULL);CREATE TABLE IF NOT EXISTS sync_pending(singleton INTEGER PRIMARY KEY,payload TEXT NOT NULL);CREATE TABLE IF NOT EXISTS sync_connection(singleton INTEGER PRIMARY KEY,address TEXT NOT NULL);CREATE TABLE IF NOT EXISTS sync_connection_head(singleton INTEGER PRIMARY KEY,hash TEXT NOT NULL);CREATE TABLE IF NOT EXISTS sync_connection_identity(address TEXT PRIMARY KEY,node_id TEXT NOT NULL);');
 db.exec('CREATE TABLE IF NOT EXISTS network_identity(singleton INTEGER PRIMARY KEY,secret TEXT NOT NULL);CREATE TABLE IF NOT EXISTS network_ledger(singleton INTEGER PRIMARY KEY,payload TEXT NOT NULL);');
 security=new BrowserIdentity(db.selectValue('SELECT secret FROM network_identity WHERE singleton=1')??'',db.selectValue('SELECT payload FROM network_ledger WHERE singleton=1')??'');
 exec('INSERT OR REPLACE INTO network_identity VALUES(1,?)',[security.saved()]);
 const persistSecurity=()=>exec('INSERT OR REPLACE INTO network_ledger VALUES(1,?)',[security.storage()]);
 auth=new NetworkAuth(security,persistSecurity,()=>self.postMessage({event:'sync_changed'}),()=>history);
 exec('INSERT OR REPLACE INTO sync_identity VALUES(1,?)',[security.id()]);db.exec('CREATE TABLE IF NOT EXISTS sync_topology_epoch(singleton INTEGER PRIMARY KEY,epoch INTEGER NOT NULL);INSERT INTO sync_topology_epoch VALUES(1,1) ON CONFLICT(singleton) DO UPDATE SET epoch=epoch+1;');topology=new BrowserTopology(identity(),Number(db.selectValue('SELECT epoch FROM sync_topology_epoch WHERE singleton=1')),performance.now());privateRoot=await(await navigator.storage.getDirectory()).getDirectoryHandle('bom-inventory-workspace',{create:true});workspace=folder||null;
 db.exec('CREATE TABLE IF NOT EXISTS inventory_applied_head(singleton INTEGER PRIMARY KEY, hash TEXT NOT NULL)');
 appliedHead=db.selectValue('SELECT hash FROM inventory_applied_head WHERE singleton=1')??'';
 revision=Number(db.selectValue('SELECT revision FROM browser_state WHERE id=1')||0);history=db.selectValues('SELECT payload FROM sync_events ORDER BY rowid').map(JSON.parse);const stored=db.selectValue('SELECT snapshot FROM browser_state WHERE id=1');
 snapshot=stored?JSON.parse(browser_storage_project_references(stored,false)):null;await recover();
 if(security.replay_membership(JSON.stringify(history.filter(isMembershipRecord))))persistSecurity();
 if(component()){
  exec("UPDATE sync_authority SET cluster='component' WHERE singleton=1");
  const draft=JSON.parse(db.selectValue('SELECT payload FROM sync_pending WHERE singleton=1')||'null');
  if(draft&&!history.some(record=>record.id===draft.record.id))componentQueue.push({...draft,legacy:true});
  else if(draft)exec('DELETE FROM sync_pending');
 }
 if(!snapshot){
  const next = legacy || (demo ? await loadDemoInventory() : JSON.stringify(empty()));
  await save({next,expected:revision,initial:true});
 }
 return state();
}
let bridge=null;

function mergeRequest(address,method,payload,blob){
 const url=new URL(address);
 if(['/raft','/raft/'].includes(url.pathname))throw Error('Raft sync has been removed. Use the WebSocket base address in Sync (/sync).');
 if(!['ws:','wss:'].includes(url.protocol)||url.username||url.password||url.search||url.hash||!['/','/sync'].includes(url.pathname))throw Error('Enter a WebSocket base address');
 url.pathname='/merge';
 return new Promise((resolve,reject)=>{
  const socket=new WebSocket(url);socket.binaryType='arraybuffer';
  const finish=(error,value)=>{clearTimeout(timer);socket.onclose=null;socket.close();if(error)reject(error);else resolve(value);};
  const timer=setTimeout(()=>finish(Error('Reconnect request timed out')),30000);
  const pairing={socket};
  socket.onopen=()=>auth.start(pairing,m=>socket.send(JSON.stringify(m)));
  socket.onmessage=({data})=>{try{if(!pairing.authenticated){if(typeof data!=='string')throw Error('Authentication required');if(auth.receive(pairing,JSON.parse(data),m=>socket.send(JSON.stringify(m)))){pairing.authenticated=true;clearTimeout(pairing.authDeadline);socket.send(JSON.stringify({method,payload}));if(blob)socket.send(blob);}return;}typeof data==='string'?finish(Error(data)):finish(null,new Uint8Array(data));}catch(e){finish(e);}};
  socket.onerror=socket.onclose=()=>finish(Error('Could not contact the desktop for merge review'));
 });
}
const mergeJson=async(...args)=>JSON.parse(decode.decode(await mergeRequest(...args)));
function mergeRecords(apply=false){
 const merged=JSON.parse(browser_branch_merge(JSON.stringify(history),JSON.stringify(bridge.branch.records),
  identity(),JSON.stringify(bridge.choices),apply));return merged.map(r=>(r.mesh?.author??r.mesh?.origin)===identity()?JSON.parse(security.sign_record(JSON.stringify(r))):r);
}
async function fetchBranch(address){
 const branch=await mergeJson(address,'branch',null);
 const choices=bridge?.address===address?bridge.choices:{};
 const known=new Set(history.map(record=>record.id));
 for(const record of branch.records){
  if(known.has(record.id))continue;
  for(const asset of assets(record)){
   if(await getBlob(asset,true))continue;
   const blob=blobDecode(await mergeRequest(address,'get_blob',asset));
   if(blob.asset!==asset)throw Error('Desktop returned an unexpected attachment');
   await rootsWrite(assetPath(asset),blob.bytes,true);
  }
 }
 bridge={address,branch,choices};
}
async function finishBranch(){
 const {address,branch}=bridge;
 const records=mergeRecords(true);
 const remoteIds=new Set(branch.records.map(record=>record.id));
 const ids=new Set(records.filter(record=>!remoteIds.has(record.id)).flatMap(assets));
 for(const id of ids)await mergeRequest(address,'blob',null,blobEncode(id,await getBlob(id)));
 const plan={id:crypto.randomUUID(),expected:branch.head},leader=branch.leader;
 try{
  await mergeJson(address,'prepare',{leader,body:plan});
  await mergeJson(address,'commit',{leader,body:{plan,records}});
 }catch(error){
  await fetchBranch(address).catch(()=>{});
  throw error;
 }finally{
  await mergeJson(address,'release',{leader,body:plan.id}).catch(()=>{});
 }
 const localIds=new Set(history.map(record=>record.id));
 const result=project(records);
 for(const record of records)if(!localIds.has(record.id))await append(record);
 commit(result);joinComponent();bridge=null;network.connect(address);exec('INSERT OR REPLACE INTO sync_connection_head VALUES(1,?)',[appliedHead]);exec('INSERT INTO sync_connection VALUES(1,?) ON CONFLICT(singleton) DO UPDATE SET address=excluded.address',[address]);
}

async function state(){const view=inspect(),draft=pending(),leader=network.leader();
 const comparisons=draft?.outcome?.result==='stale'?proposalReviews(draft):[];
 const graph=topology?JSON.parse(topology.graph(performance.now())):null;if(graph){
  graph.managed=!!component();if(leader)graph.leader=leader;
 }if(hydratedBase!==snapshot){hydratedText=snapshot?JSON.stringify(await hydrate(snapshot)):null;hydratedBase=snapshot;}return {sync:{security:auth?.state(),topology:graph,bridge:bridge?{address:bridge.address,reviews:mergeRecords()}:null,component:component(),queued:componentQueue.length,leader,pending:draft?{outcome:draft.outcome,id:draft.record.id,comparisons,choices:draft.choices??{},ready:proposalReady(draft,comparisons)}:null,applicationId:identity(),head:view.head,appliedHead,sharedHead:view.sharedHead,reviews:(component()?[]:view.reviews).map((r,i)=>({...r,draft:db.selectValue('SELECT text FROM sync_merge_drafts WHERE id=?',[r.id])??JSON.stringify(view.drafts[i],null,2)})),warnings:view.warnings,peers:network.statuses(),pulling:network.pulling()},snapshot:hydratedText,revision,applicationId:db.selectValue('SELECT application_id FROM sync_identity WHERE singleton=1'),workspace:workspace?workspace.name:null,needsPermission:!!workspace&&!await workspaceWritable(workspace)};}
async function save({next,expected,initial=false}){if(!security.trusted(identity()))throw Error('This node has been revoked');if(inspect().reviews.length)throw Error('Resolve the pending comparisons in Sync before editing.');if(expected!==revision)throw Error('Inventory changed while saving. Reload before editing.');await permission(workspace);const data=JSON.parse(next);for(const p of data.projects||[]){p.id||=crypto.randomUUID().toUpperCase();p.notesAttachmentID||='project-'+crypto.randomUUID().toUpperCase();}
 const canonical=await externalize(data,snapshot);let record=JSON.parse(browser_storage_make(JSON.stringify(snapshot||empty()),JSON.stringify(canonical),JSON.stringify(history),component()?componentOrigin:identity()));record.initial=initial;
 if(component())record.mesh.author=identity();
 record=JSON.parse(security.sign_record(JSON.stringify(record)));
 if(!network.connected()&&pending()){
  if(record.mesh.mutations.length)componentQueue.push({record,local:canonical,outcome:{result:'unavailable'}});
  await settleOfflineQueue();
  if(!record.mesh.mutations.length)commit(project(history,canonical));
  return state();
 }

 if(record.mesh.mutations.length&&component()&&network.connected()){
  const draft={record,local:canonical,outcome:{result:'unavailable'}};
  if(componentQueue.length>=128)throw Error('Shared save: The pending edit queue is full.');
  componentQueue.push(draft);network.changed();throw Error('Shared save: Edit queued for leader approval.');
 }
 if(record.mesh.mutations.length){const result=project([...history,record],canonical);await append(record);commit(result);network.changed();}else{commit(project(history,canonical));}return state();}
async function copyTree(from,to,path){let dir;try{dir=await from.getDirectoryHandle(path);}catch(e){if(e.name==='NotFoundError')return;throw e;}async function visit(handle,prefix){for await(const[name,entry]of handle.entries()){const relative=prefix+'/'+name;if(entry.kind==='directory')await visit(entry,relative);else if(path==='Attachments'||name.endsWith('.wal')){const file=await entry.getFile();if(file.size>72*1024*1024)throw Error('Workspace file exceeds size limit');await immutable(to,relative,new Uint8Array(await file.arrayBuffer()));}}}await visit(dir,path);}
async function bind({folder}){await permission(folder);const records=union(await chunks(folder));
 if(component()&&records.some(record=>!history.some(known=>known.id===record.id)))throw Error('Choose a folder without another inventory history.');const result=records.length?project(records):null;
 for(const path of ['Attachments','inventory.actions']){await copyTree(folder,privateRoot,path);await copyTree(privateRoot,folder,path);}if(result)commit(result);workspace=folder;activeChunk=null;return state();}
async function resetInventory(){
 if(!db||!pool)throw Error('Browser inventory is unavailable. Reload and try again.');
 for(const peer of network.statuses())network.disconnect(peer.id);
 workspace=null;
 const root=await navigator.storage.getDirectory();
 db.close();db=null;
 await pool.removeVfs();pool=null;
 // removeVfs may log a cleanup error without rejecting.
 for(const name of ['.bom-inventory-sqlite','bom-inventory-workspace']){
  try{await root.removeEntry(name,{recursive:true});}
  catch(error){if(error.name!=='NotFoundError')throw error;}
 }
 return true;
}
async function backup(){const bytes=sql.capi.sqlite3_js_db_export(db.pointer);if(workspace){await permission(workspace);await write(workspace,`backups/inventory-${Date.now()}.sqlite`,bytes);}return bytes;}

let queue=Promise.resolve();
function enqueue(fn){const task=queue.then(fn);queue=task.catch(()=>{});return task;}
function assets(record){return JSON.parse(browser_sync_assets(JSON.stringify(record)));}
async function getBlob(id,optional=false){const path=assetPath(id);let bytes=await exists(privateRoot,path);if(!bytes&&workspace){bytes=await exists(workspace,path);if(bytes)await immutable(privateRoot,path,bytes);}if(!bytes&&!optional)throw Error(`Missing attachment: ${path}`);return bytes;}
function proposalReviews(draft){return JSON.parse(browser_proposal_reviews(JSON.stringify(history),JSON.stringify(draft.record)));}
function proposalReady(draft,reviews){
 return draft?.outcome?.result==='stale'&&draft.outcome.detail.every(([entity,key,field])=>
  reviews.some(review=>review.entity===entity&&review.key===key&&(review.field===field||review.field==='$entity')));
}
async function resolveDraft(id,value){
 const draft=pending();if(!draft)throw Error('No draft is pending');
 const comparisons=proposalReviews(draft);
 if(!proposalReady(draft,comparisons)||!comparisons.some(review=>review.id===id))throw Error('Wait for the latest shared values and review the comparison again.');
 const choices={...draft.choices,[id]:value};
 if(comparisons.some(review=>!Object.hasOwn(choices,review.id))){savePending({...draft,choices});return state();}
 const record=JSON.parse(browser_rebase_proposal(JSON.stringify(history),JSON.stringify(draft.record),JSON.stringify(choices)));
 if(!record.mesh.mutations.length){savePending(null);return state();}
 const next={record,local:snapshot,outcome:{result:'unavailable'},legacy:draft.legacy};savePending(next);return submitDraft(next);
}
// A disconnected browser owns its local branch. Preserve a sent proposal's
// immutable record so a late desktop approval can be deduplicated on reconnect.
async function settleOfflineQueue(){
 if(network.connected())return;
 while(pending()){
  const draft=pending();
  if(history.some(record=>record.id===draft.record.id)){savePending(null);continue;}
  let record=draft.record;
  if(!draft.prepared){
   const choices=Object.fromEntries(proposalReviews(draft).map(review=>[review.id,review.ours]));
   record=JSON.parse(browser_rebase_proposal(JSON.stringify(history),JSON.stringify(record),JSON.stringify(choices)));
   record.id=draft.record.id;
   if(!record.mesh.mutations.length){savePending(null);continue;}
  }
  await accept(record,draft.local);
 }
 network.changed();
}
async function connectWithReview(address){
 await settleOfflineQueue();
 try{await fetchBranch(address);}
 catch(error){
  if(component()||error.message!=='This desktop does not use Partition sync')throw error;
  network.connect(address);
  exec('INSERT INTO sync_connection VALUES(1,?) ON CONFLICT(singleton) DO UPDATE SET address=excluded.address',[address]);
  return;
 }
 if(!mergeRecords().length)await finishBranch();
}

async function submitDraft(draft){
 if(component()&&!draft.prepared){
  const sequence=(inspect().clock[draft.record.mesh.origin]??0)+1;
  if(draft.record.mesh.sequence!==sequence){
   const reviews=proposalReviews(draft);
   if(reviews.length){savePending({...draft,outcome:{result:'stale',detail:reviews.map(r=>[r.entity,r.key,r.field])}});network.changed();return state();}
   const record=JSON.parse(browser_rebase_proposal(JSON.stringify(history),JSON.stringify(draft.record),'{}'));
   if(!record.mesh.mutations.length){savePending(null);network.changed();return state();}
   record.id=draft.record.id;draft={...draft,record};
  }
  draft={...draft,record:JSON.parse(security.sign_record(JSON.stringify(draft.record))),prepared:true};savePending(draft);
 }
 const outcome=await network.propose(draft.record).catch(()=>({result:'unavailable'}));
 if(outcome?.result==='committed'){
  const approved=outcome.record??draft.record;
  await accept(approved,draft.local);network.changed();return state();
 }
 savePending({...draft,outcome});network.changed();
 const message=outcome?.result==='stale'?'The shared inventory changed. Review your draft in Sync.':outcome?.result==='invalid'?String(outcome.detail):'Waiting for a leader. Your edit is queued in Sync.';
 throw Error('Shared save: '+message);
}
function isMembershipRecord(record){return record.actions?.some(action=>action.entity==='network_membership');}
async function accept(record,local=snapshot){if(record.mesh?.author===identity()||record.mesh?.origin===identity()){record=JSON.parse(security.sign_record(JSON.stringify(record)));}security.verify_record(JSON.stringify(record));if(record.version!==2||!record.mesh)throw Error('Mesh sync requires version 2 events');await permission(workspace);for(const id of assets(record))if(!await getBlob(id,true))throw Error('Event arrived before its attachment');const old=history.find(r=>r.id===record.id);const result=project([...history,record],local);if(!old){await append(record);commit(result);}if(isMembershipRecord(record)&&security.replay_membership(JSON.stringify([record])))auth.save();if(pending()?.record.id===record.id)savePending(null);}
const network=new SyncNetwork({enqueue,identity,
 get auth(){return auth;},component,joinComponent,view:()=>inspect(),assets,
 topologyAds:()=>topology?JSON.parse(topology.ads(performance.now())):[],
 topologyConnect:id=>topology.connect(id,performance.now()),topologyDisconnect:id=>topology.disconnect(id,performance.now()),
 topologyReceive:ads=>topology.receive(JSON.stringify(ads),performance.now()),topologyTick:()=>topology?.heartbeat(performance.now()),
 encode:(r,proposal=false)=>browser_sync_encode(JSON.stringify(r),proposal),decode:bytes=>JSON.parse(browser_sync_decode(bytes)),
 getBlob,putBlob:(id,bytes)=>rootsWrite(assetPath(id),bytes,true),receive:accept,
 currentAssets:()=>{const ids=new Set();function visit(v){if(typeof v==='string'&&v.startsWith('asset:')){assetPath(v);ids.add(v);}else if(Array.isArray(v))v.forEach(visit);else if(v&&typeof v==='object')Object.values(v).forEach(visit);}const shared=structuredClone(snapshot);if(shared){for(const p of shared.projects||[])delete p.kicad;shared.bom=(shared.bom||[]).filter(b=>b.kicad_source==null);}visit(shared);return ids;},
 notePeer:async(id,_clock)=>exec('INSERT INTO sync_peers VALUES(?,?,?) ON CONFLICT(id) DO UPDATE SET last_received_ms=excluded.last_received_ms,clock=excluded.clock',[id,Date.now(),JSON.stringify(inspect().clock)]),
 bundleReady:()=>self.postMessage({event:'git_bundle_ready'}),
 notify:()=>self.postMessage({event:'sync_changed'})
});
// Reuse the last explicitly joined gateway after a reload or transient loss.
// A deliberate Disconnect clears this setting. Offline edits require a manual
// reconnect so their author can review the branch comparison.
setInterval(()=>{
 if(!db)return;
 enqueue(async()=>{
  if(bridge||network.statuses().some(peer=>peer.active))return;
  const address=db.selectValue('SELECT address FROM sync_connection WHERE singleton=1');
  const joinedHead=db.selectValue('SELECT hash FROM sync_connection_head WHERE singleton=1');
  if(address&&joinedHead===appliedHead&&!pending()&&network.canReconnect(address))network.connect(address,db.selectValue('SELECT node_id FROM sync_connection_identity WHERE address=?',[address])??null);
 }).catch(()=>{});
},5000);
let dispatching=false;
setInterval(()=>{
 if(!db||!component()||dispatching||!pending())return;
 dispatching=true;enqueue(async()=>{
  if(!network.connected()){await settleOfflineQueue();return;}
  const draft=pending();
  if(draft?.outcome.result==='unavailable'&&network.leader())await submitDraft(draft);
 }).catch(()=>{}).finally(()=>{dispatching=false;});
},700);
self.onmessage=({data})=>{enqueue(async()=>{try{let value;switch(data.op){case'init':value=await init(data);break;case'save':value=await save(data);break;case'bind':value=await bind(data);break;case'backup':value=await backup();break;case'state':value=await state();break;
 case 'reset_inventory':value=await resetInventory();break;
 case 'git_bundle_peers':value=network.bundlePeers();break;
 case 'apply_git_snapshot':{
  if(!inspect().reviews.length){
   const local=JSON.parse(browser_apply_git_snapshot(JSON.stringify(snapshot),data.projectId,data.tip,JSON.stringify(data.selection)));
   if(JSON.stringify(local)!==JSON.stringify(snapshot))commit(project(history,local));
  }
  value=await state();break;
 }
 case 'history':
  value = {
   page: await historyPage(data.before, data.limit),
   author: identity(),
   conflicts: inspect().reviews.flatMap(review => review.events)
  };
  break;
 case'authorize':
 case'revoke':{
  const revoke=data.op==='revoke';
  if(!component()&&!revoke){
   // This local trust anchor lets the browser authenticate its first gateway.
   auth.authorize(data.public_key,false);
  }else{
   if(!network.leader())throw Error('Connect to the sync leader to change membership');
   if(componentQueue.length)throw Error('Wait for pending edits before changing membership');
   const inventory=JSON.stringify(snapshot||empty());
   let record=JSON.parse(browser_storage_make(inventory,inventory,JSON.stringify(history),componentOrigin));
   record.mesh.author=identity();
   record.actions=[{entity:'network_membership_request',kind:revoke?'revoke':'approve',key:'',label:'',value:data.public_key,changes:[],position:null}];
   record=JSON.parse(security.sign_record(JSON.stringify(record)));
   const outcome=await network.propose(record);
   if(outcome.result!=='committed')throw Error(outcome.detail||'The sync leader could not commit the membership change');
  }
  network.changed();value=await state();break;
 }
 case'connect':{
  if(network.statuses().some(peer=>peer.active))throw Error('Disconnect the current desktop before connecting another one.');
  network.connect(data.url,data.expected_id??null,true);
  if(data.expected_id)exec('INSERT OR REPLACE INTO sync_connection_identity VALUES(?,?)',[data.url,data.expected_id]);
  exec('INSERT OR REPLACE INTO sync_connection VALUES(1,?)',[data.url]);
  value=await state();break;
 }
 case'bridge_open':await fetchBranch(data.url);if(!mergeRecords().length)await finishBranch();value=await state();break;
 case'bridge_refresh':if(!bridge)throw Error('Open a connection first');await fetchBranch(bridge.address);value=await state();break;
 case'bridge_cancel':bridge=null;value=await state();break;
 case'bridge_finish':if(!bridge)throw Error('Open a connection first');await finishBranch();value=await state();break;
 case'bridge_resolve':{
  if(!bridge||!mergeRecords().some(review=>review.id===data.conflict))throw Error('Refresh the comparison');
  bridge.choices[data.conflict]=data.value;value=await state();break;
 }
 case'disconnect':exec('DELETE FROM sync_connection');network.disconnect(data.peer);value=await state();break;
 case'retry_proposal':{const draft=pending();if(!draft||draft.outcome.result!=='unavailable')throw Error('No draft is waiting for confirmation');value=await submitDraft(draft);break;}
 case'discard_proposal':{const draft=pending();if(draft?.outcome?.result==='unavailable')throw Error('Retry first to check whether the draft already committed.');savePending(null);value=await state();break;}
 case'resolve':{if(component()){value=await resolveDraft(data.conflict,data.value);break;}const r=JSON.parse(browser_sync_resolve(JSON.stringify(history),identity(),data.conflict,JSON.stringify(data.value)));await accept(r);exec('DELETE FROM sync_merge_drafts WHERE id=?',[data.conflict]);network.changed();value=await state();break;}
 case'draft':exec('INSERT INTO sync_merge_drafts VALUES(?,?) ON CONFLICT(id) DO UPDATE SET text=excluded.text',[data.conflict,data.text]);value=true;break;
 default:throw Error('Unknown storage operation');}self.postMessage({id:data.id,value});}catch(e){self.postMessage({id:data.id,error:String(e.message||e)});}});};

// Browser file chooser: bounded input, with listeners and DOM nodes released on
// both selection and cancellation. The promise starts during the user's click.
export function pickFile(accept) {
  return new Promise((resolve, reject) => {
    const input = document.createElement('input');
    input.type = 'file'; input.accept = accept; input.hidden = true;
    const finish = value => { input.remove(); resolve(value); };
    input.oncancel = () => finish(null);
    input.onchange = async () => {
      try {
        const file = input.files?.[0];
        if (!file) return finish(null);
        if (file.size > 5_000_000) throw Error('Choose a file smaller than 5 MB for this browser preview.');
        const bytes = new Uint8Array(await file.arrayBuffer());
        finish(bytes);
      } catch (error) { input.remove(); reject(error); }
    };
    document.body.append(input); input.click();
  });
}

const keepGitBundlesKey = 'vinventory.keep-git-bundles';
export function keepGitBundles() { return localStorage.getItem(keepGitBundlesKey) === 'true'; }
export function setKeepGitBundles(keep) { localStorage.setItem(keepGitBundlesKey, String(keep)); }

const gitSnapshotKey = (projectId, tip) => `git-snapshot:${projectId}:${tip}`;
const gitFetches = new Map();

const maxGitTransferBytes=4*1024*1024*1024;
async function removeGitJournal(name){if(!name)return;const root=await navigator.storage.getDirectory();await(await root.getDirectoryHandle('git-work')).removeEntry(name).catch(error=>{if(error.name!=='NotFoundError')console.warn('Git journal cleanup:',error);});}
async function streamGit(request){
  const worker=new Worker(new URL('./worker.js',document.baseURI),{type:'module'});
  try{return await new Promise((resolve,reject)=>{
    worker.onmessage=({data})=>data.error?reject(Error(data.error)):resolve(data);
    worker.onerror=event=>reject(Error(event.message||'Git streaming worker failed'));
    worker.postMessage({gitStream:request});
  });}finally{worker.terminate();}
}
function gitRange(file,range){if(!range||!Number.isSafeInteger(range.offset)||!Number.isSafeInteger(range.length)||range.offset<0||range.length<0||range.offset+range.length>file.size)throw Error('Invalid staged Git range');return file.slice(range.offset,range.offset+range.length);}
async function writeGitRange(target,file,range){const writer=await target.createWritable();try{await gitRange(file,range).stream().pipeTo(writer);}catch(error){await writer.abort().catch(()=>{});throw error;}}
async function immutableGitPayload(directory,name,journal,range){
  try{const existing=await directory.getFileHandle(name);if((await existing.getFile()).size===range.length)return existing;}
  catch(error){if(error.name!=='NotFoundError')throw error;}
  const target=await directory.getFileHandle(name,{create:true});await writeGitRange(target,journal,range);return target;
}
async function cacheGitSnapshot(projectId,tip,value,journal){
  const root=await navigator.storage.getDirectory(),snapshots=await root.getDirectoryHandle('git-kicad-snapshots',{create:true});
  const folder=crypto.randomUUID(),checkout=await snapshots.getDirectoryHandle(folder,{create:true});
  try{
    for(const [path,range]of Object.entries(value.snapshot)){
      const components=path.split('/'),name=components.pop();if(components.some(p=>!p||p==='..')||!name)throw Error('Invalid Git snapshot path');
      let directory=checkout;for(const part of components)directory=await directory.getDirectoryHandle(part,{create:true});
      await writeGitRange(await directory.getFileHandle(name,{create:true}),journal,range);
    }
    const selection={folder,choices:value.choices,files:{},created_at:Date.now()};
    await putRecord('folders',folder,checkout);await putRecord('inventory',gitSnapshotKey(projectId,tip),selection);return selection;
  }catch(error){await snapshots.removeEntry(folder,{recursive:true}).catch(()=>{});throw error;}
}
async function keepRawGitBundle(cache,file,bases,tips){
  const range=JSON.stringify({bases,tips}),digest=await crypto.subtle.digest('SHA-256',new TextEncoder().encode(range));
  const key=Array.from(new Uint8Array(digest),b=>b.toString(16).padStart(2,'0')).join('');
  await file.stream().pipeTo(await(await cache.getFileHandle(key+'.bundle',{create:true})).createWritable());
  const writer=await(await cache.getFileHandle(key+'.json',{create:true})).createWritable();await writer.write(range);await writer.close();
}
async function importGitBundles(...args){return withGitProcessing(()=>importGitBundlesInner(...args));}
async function importGitBundlesInner(selected,projectId,transfer=null,retain=false){
  const root=await navigator.storage.getDirectory(),bundles=await root.getDirectoryHandle('git-bundles',{create:true}),cache=await bundles.getDirectoryHandle(projectId,{create:true});
  const files={},imports=[],cached=[];
  selected.forEach((file,index)=>{if(file.size>maxGitTransferBytes)throw Error('Git bundle exceeds the 4 GiB transfer limit');const key='\0selected/'+index;files[key]=file;imports.push(key);});
  for await(const entry of cache.values())if(entry.kind==='file'&&entry.name.endsWith('.bundle')){const key='\0cached/'+entry.name;files[key]=await entry.getFile();cached.push(key);}
  gitUpdate(projectId,'Verifying and extracting history',transfer?.tip??'');
  const result=await streamGit({files,imports,cached,operation:'extract',tip:transfer?.tip,expected_sha256:transfer?.sha256});
  try{
    const value=result.value,tip=value.selected_tip,preserve=retain||keepGitBundles();
    if(preserve)for(const range of value.ranges)await keepRawGitBundle(cache,selected[range.index],range.bases,range.tips);
    gitUpdate(projectId,'Preparing files',tip);
    const selection=await cacheGitSnapshot(projectId,tip,value,result.stage);
    if(preserve)localStorage.setItem('vinventory.git-base:'+projectId,tip);
    return JSON.stringify(selection);
  }finally{await removeGitJournal(result.temporary);}
}

const gitReceiveProgress=new Map();
let gitReceiveReporter=null;
function reportGitReceive(id,received,total){
  if(total)gitReceiveProgress.set(id,{received,total});else gitReceiveProgress.delete(id);
  if(gitReceiveReporter){let received=0,total=0;for(const value of gitReceiveProgress.values()){received+=value.received;total+=value.total;}gitReceiveReporter(received,total,gitReceiveProgress.size);}
}
const gitApplyingProgress=new Set(),gitCreatingProgress=new Set();
async function withGitActivity(active,reporterName,work){
  const id=Symbol();let reporter;
  try{reporter=(await import(new URL('./pkg/vispace.js',document.baseURI)))[reporterName];}catch{}
  const report=()=>{try{reporter?.(active.size);}catch{}};
  active.add(id);report();
  try{return await work();}finally{active.delete(id);report();}
}
function withGitProcessing(work){return withGitActivity(gitApplyingProgress,'report_git_bundle_applying',work);}
function withGitCreating(work){return withGitActivity(gitCreatingProgress,'report_git_bundle_creating',work);}
async function downloadGitBundle(peer, projectId, handle, base, tip) {
  const url = new URL('/git-bundle', peer.url);
  const writer = await handle.createWritable();
  const progressId=Symbol();let progressAt=0;
  // The module is already initialized by the app; tests can load the host alone.
  try{gitReceiveReporter=(await import(new URL('./pkg/vispace.js',document.baseURI))).report_git_bundle_receive;}catch{}
  let queue = Promise.resolve(), header, received = 0;
  try {
    await new Promise((resolve, reject) => {
      const socket = new WebSocket(url);
      socket.binaryType = 'arraybuffer';
      let timer, current = false;
      const fail = error => { clearTimeout(timer); reject(error); socket.close(); };
      const reset = () => { clearTimeout(timer); timer = setTimeout(()=>fail(Error('Git bundle transfer timed out.')), 60000); };
      reset();
      socket.onopen = () => socket.send(JSON.stringify({project:projectId, grant:peer.grant, base, tip, flow:true}));
      socket.onerror = () => fail(Error('Could not connect to the peer bundle endpoint.'));
      socket.onmessage = ({data}) => {
        reset();
        try {
          if (typeof data === 'string') {
            const message = JSON.parse(data);
            if (message.error) throw Error(message.error);
            if (message.current === true && !header) { current = true; return; }
            if (header || !Number.isSafeInteger(message.size) || message.size < 1 || message.size > maxGitTransferBytes
                || !/^(?:[a-f0-9]{40}|[a-f0-9]{64})$/.test(message.tip)
                || !/^[a-f0-9]{64}$/.test(message.sha256))
              throw Error('Invalid Git bundle transfer header.');
            header = message;reportGitReceive(progressId,0,header.size);
          } else {
            received += data.byteLength;
            if (!header || data.byteLength > 64*1024 || received > header.size)
              throw Error('Invalid Git bundle frame.');
            const credit=received;
            queue = queue.then(async()=>{await writer.write(data);if(credit===header.size||performance.now()-progressAt>=100){reportGitReceive(progressId,credit,header.size);progressAt=performance.now();}if(credit%(1024*1024)===0||credit===header.size)socket.send(JSON.stringify({received:credit}));});
            queue.catch(fail);
          }
        } catch (error) { fail(error); }
      };
      socket.onclose = () => {
        clearTimeout(timer);
        if (current) resolve();
        else if (!header || received !== header.size) reject(Error('Git bundle transfer ended early.'));
        else queue.then(resolve,reject);
      };
    });
    await writer.close();
    gitUpdate(projectId,'Received bytes',String(received));
    return header ?? null;
  } catch (error) {
    await queue.catch(()=>{});
    await writer.abort().catch(()=>{});
    throw error;
  } finally {reportGitReceive(progressId,0,0);}
}

async function fetchGitSnapshot(projectId, requestedTip, peers, {retain = false, retry = false} = {}) {
  const key = requestedTip ? gitSnapshotKey(projectId, requestedTip) : null;
  if (key) {
    const cached = await readRecord('inventory', key);
    if (cached && await readRecord('folders', cached.folder)) return JSON.stringify(cached);
    if (gitFetches.has(key)) return gitFetches.get(key);
  }
  const job = (async () => {
    if (!peers.length) throw Error('Connect to a desktop peer in Sync before fetching a Git bundle.');
    const root = await navigator.storage.getDirectory();
    const stage = await root.getDirectoryHandle('git-downloads', {create:true});
    const base = localStorage.getItem('vinventory.git-base:' + projectId);
    let failure;
    for (const delay of retry ? [0, 2000, 5000] : [0]) {
      if (delay) await new Promise(resolve=>setTimeout(resolve,delay));
      for (const peer of peers) {
        const name = crypto.randomUUID() + '.bundle';
        try {
          const handle = await stage.getFileHandle(name, {create:true});
          // An advertised commit may already be our base even if its snapshot was removed.
          const transfer = await downloadGitBundle(peer, projectId, handle, base === requestedTip ? null : base, requestedTip);
          if (transfer === null) return null;
          if (requestedTip && transfer.tip !== requestedTip) throw Error('Peer sent a different Git commit.');
          return await importGitBundles([await handle.getFile()], projectId, transfer, retain);
        } catch (error) { failure = error; }
        finally { await stage.removeEntry(name).catch(()=>{}); }
      }
    }
    throw failure;
  })();
  if (key) gitFetches.set(key, job);
  try { return await job; }
  finally { if (key) gitFetches.delete(key); }
}

export async function fetchGitBundle(projectId, requestedTip) {
  return fetchGitSnapshot(projectId, requestedTip, await sqliteStorage.call('git_bundle_peers'), {retry:true});
}

const gitUpdates=new Map();
function gitUpdate(project,phase,detail){
  const events=gitUpdates.get(project)??[];const last=events.at(-1);
  if(last?.phase!==phase||last?.detail!==detail){events.push({time:new Date().toISOString(),phase,detail});if(events.length>30)events.shift();}
  gitUpdates.set(project,events);
  if(['Blocked','Conflict','Applied'].includes(phase))import(new URL('./pkg/vispace.js',document.baseURI)).then(wasm=>wasm.report_git_sync_status(project,phase!=='Applied')).catch(()=>{});
}
async function gitDiagnostics(projectId,limit=100){
  if(!sqliteStorage&&globalThis.__inventorySqliteBridge)return globalThis.__inventorySqliteBridge.gitDiagnostics(projectId);
  const project=JSON.parse(inventorySnapshot()).projects?.find(p=>p.id===projectId);
  if(!project)throw Error('Project no longer exists');
  const result={events:gitUpdates.get(projectId)??[],head:null,history:[],bundles:[],folder:project.kicad?.browser_folder??null,peers:(await sqliteStorage.call('git_bundle_peers')).map(peer=>peer.url),pending_comparisons:sqliteStorage.state.sync?.reviews?.length??0,connections:(await sqliteStorage.call('state')).sync?.peers??[]};
  const root=await navigator.storage.getDirectory();
  const files={},imports=[];
  try{const cache=await(await root.getDirectoryHandle('git-bundles')).getDirectoryHandle(projectId);
    for await(const entry of cache.values())if(entry.kind==='file'&&entry.name.endsWith('.json')){
      result.bundles.push(JSON.parse(await(await entry.getFile()).text()));
    }
    for await(const entry of cache.values())if(entry.kind==='file'&&entry.name.endsWith('.bundle')){const key='\0history/'+entry.name;files[key]=await entry.getFile();imports.push(key);}
  }catch(error){if(error.name!=='NotFoundError')result.error=String(error);}
  if(result.folder){try{const log=await runLocalGit(result.folder,projectId,'log','','','',{history_limit:limit+1});result.head=log.head;result.history=log.history??[];result.writable=log.initialized===true;}catch(error){result.error=String(error);}}
  if(!result.history.length&&imports.length){
    const {latestGitTips}=await import(new URL('./git-snapshot.js',document.baseURI));const tips=latestGitTips(project);
    if(tips.length===1){let log;try{log=await streamGit({files,imports,operation:'bundle_log',tip:tips[0],history_limit:limit+1});result.history=log.value.history;result.history_source='Received bundle (not checkout)';}catch(error){result.error=String(error);}finally{if(log)await removeGitJournal(log.temporary);}}
  }
  result.history_more=result.history.length>limit;result.history=result.history.slice(0,limit);
  return result;
}

let gitPollBusy = false, gitPollAgain = false, gitPollCursor = 0;
async function importReceivedRefs(project,tip){
 const folder=project.kicad?.browser_folder;if(!folder)return;
 const directory=await readRecord('folders',folder);if(!directory)return;
 // Extracted read-only snapshots have no .git. User-linked repositories do.
 let git;try{git=await directory.getDirectoryHandle('.git');}catch(error){if(error.name==='NotFoundError')return;throw error;}
 try{const refs=await(await git.getDirectoryHandle('refs')).getDirectoryHandle('vinventory');const bundle=await refs.getDirectoryHandle('bundles');if((await(await(await bundle.getFileHandle(tip)).getFile()).text()).trim()===tip)return;}catch(error){if(error.name!=='NotFoundError')throw error;}
 gitUpdate(project.id,'Importing history',tip);
 await withGitProcessing(()=>runLocalGit(folder,project.id,'import','','',''));
 gitUpdate(project.id,'History available in refs',tip);
}

async function publishLocalGitTips(){
 for(const project of JSON.parse(snapshot).projects??[]){
  const folder=project.kicad?.browser_folder;if(!folder)continue;
  try{
   const head=await folderGitHead(folder);if(!head||Object.values(project.git_tips??{}).some(tip=>tip.commit===head))continue;
   const result=await runLocalGit(folder,project.id,'share','','','');
   if(result.tip){const wasm=await import(new URL('./pkg/vispace.js',document.baseURI));wasm.report_local_git_tip(project.id,JSON.stringify(result));gitUpdate(project.id,'Announcing local commit',result.tip);}
  }catch(error){gitUpdate(project.id,'Local history pending',String(error));}
 }
}

async function pollGitBundles() {
  if(gitPollBusy){gitPollAgain=true;return;}
  if (resetting || !snapshot || !sqliteStorage || storageFailure) return;
  gitPollBusy = true;
  gitPollAgain = false;
  try {
    const {latestGitTips} = await import(new URL('./git-snapshot.js', document.baseURI));
    const peers = await sqliteStorage.call('git_bundle_peers');
    if(!peers.length)for(const project of JSON.parse(snapshot).projects??[])if(Object.keys(project.git_tips??{}).length)gitUpdate(project.id,'Waiting for connection','No live bundle source. Open Sync to inspect the gateway connection.');
    await publishLocalGitTips();
    await uploadLocalGitBundles(peers);
    const pending = [];
    for (const project of JSON.parse(snapshot).projects ?? []) {
      if (!/^[a-f0-9-]{36}$/i.test(project.id)) continue;
      const tips = latestGitTips(project);
      for (const tip of tips) {
        const key = gitSnapshotKey(project.id, tip);
        const cached = await readRecord('inventory', key);
        if (cached && await readRecord('folders', cached.folder)) {
          try{
            await importReceivedRefs(project,tip);
            if(tips.length===1)await applyGitSnapshot(project.id, tip, cached);
            else gitUpdate(project.id,'Conflict','Competing commits retained in refs; resolve locally and commit the merged history.');
          }catch(error){gitUpdate(project.id,'Blocked',String(error));persistenceStatus('Git update pending: '+error.message);}
        } else if (!cached && !gitFetches.has(key))
          pending.push({projectId:project.id, tip, retain:tips.length > 1});
      }
    }
    if (pending.length && peers.length) {
      const {projectId, tip, retain} = pending[gitPollCursor++ % pending.length];
      gitUpdate(projectId,'Receiving',tip);
      try{
        const selection = await fetchGitSnapshot(projectId, tip, peers, {retain:true});
        gitUpdate(projectId,'Received',tip);
        if(selection){const project=JSON.parse(snapshot).projects?.find(p=>p.id===projectId);if(project)await importReceivedRefs(project,tip);if(!retain)await applyGitSnapshot(projectId,tip,JSON.parse(selection));else gitUpdate(projectId,'Conflict','Competing commits retained in refs; resolve locally and commit the merged history.');}
      }catch(error){gitUpdate(projectId,'Blocked',String(error));throw error;}
    }
    await cleanupGitStorage();
  } catch (error) {
    console.warn("Automatic Git snapshot update will retry:", error);
    persistenceStatus(`Git update pending: ${error.message || error}`);
  } finally { gitPollBusy = false; if(gitPollAgain){gitPollAgain=false;queueMicrotask(()=>pollGitBundles());} }
}

// Read just refs, not the worktree: polling an already applied physical repo
// must not rehash every model, nor trust a legacy projection's revision alone.
let lastGitCleanup=0;
// Cache-only GC: user directories, Git objects, refs, and working files stay intact.
export async function cleanupGitStorage({force=false,minAge=600000}={}){
  if(!sqliteStorage||!snapshot||resetting||gitFetches.size)return {bundles:0,uploads:0,snapshots:0};
  if(!force&&Date.now()-lastGitCleanup<60000)return {bundles:0,uploads:0,snapshots:0};
  lastGitCleanup=Date.now();
  const root=await navigator.storage.getDirectory(),data=JSON.parse(snapshot),count={bundles:0,uploads:0,snapshots:0};
  const {latestGitTips}=await import(new URL('./git-snapshot.js',document.baseURI));
  const {redundantGitRanges}=await import(new URL('./git-cache-cleanup.js',document.baseURI));
  const optional=async(parent,name)=>{try{return await parent.getDirectoryHandle(name);}catch(error){if(error.name==='NotFoundError')return null;throw error;}};
  const old=async(entry)=>Date.now()-(await entry.getFile()).lastModified>=minAge;
  const records=await gitCacheRecords(),protectedFolders=new Set((data.projects??[]).map(p=>p.kicad?.browser_folder).filter(Boolean));
  const protectedUploads=new Set();
  for(const [key,value]of records){
    if(key.startsWith('git-outbox:')){if(value?.file)protectedUploads.add(value.file);if(value?.deltaFile)protectedUploads.add(value.deltaFile);}
    if(key.startsWith('git-snapshot:')&&value?.folder){
      const project=(data.projects??[]).find(p=>key.startsWith('git-snapshot:'+p.id+':'));
      if(project&&(latestGitTips(project).length!==1||Object.values(project.git_tips??{}).some(t=>key===gitSnapshotKey(project.id,t.commit))))protectedFolders.add(value.folder);
    }
  }
  const outbox=await optional(root,'git-outbox');
  for(const project of data.projects??[]){
    const folder=project.kicad?.browser_folder,tips=latestGitTips(project);
    if(!folder||tips.length!==1)continue;
    try{
      // Prove ancestry with commit metadata before deciding whether a baseline is needed.
      const cacheRoot=await optional(root,'git-bundles'),cache=cacheRoot&&await optional(cacheRoot,project.id),ranges=[];
      if(cache)for await(const entry of cache.values())if(entry.kind==='file'&&entry.name.endsWith('.json')){
        try{const range=JSON.parse(await(await entry.getFile()).text());const payload=await cache.getFileHandle(entry.name.slice(0,-5)+'.bundle');if(Array.isArray(range.bases)&&Array.isArray(range.tips))ranges.push({...range,metadata:entry.name,payload});}catch{}
      }
      const candidates=[...new Set(ranges.flatMap(r=>r.tips))];
      const proof=await runLocalGit(folder,project.id,'reachable','','','',{known_tips:candidates});
      if(proof.head!==tips[0]||!proof.initialized)continue;
      if(cache&&!keepGitBundles()&&!ranges.some(r=>r.bases.length===0&&r.tips.includes(proof.head))){
        // A fresh full baseline makes older incremental chains disposable, and
        // is generated entirely from the already imported repository objects.
        if(!await Promise.all(ranges.filter(r=>!r.tips.includes(proof.head)).map(r=>old(r.payload))).then(ages=>ages.some(Boolean)))continue;
        const rebuilt=await runLocalGit(folder,project.id,'cache','','','',{known_tips:candidates});
        if(rebuilt.head!==proof.head)continue;
        const key=Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256',new TextEncoder().encode(JSON.stringify({bases:[],tips:[proof.head]})))),b=>b.toString(16).padStart(2,'0')).join('');
        ranges.push({bases:[],tips:[proof.head],metadata:key+'.json',payload:await cache.getFileHandle(key+'.bundle')});
      }
      await navigator.locks.request('vinventory-local-git:'+folder,async()=>{
        if(await folderGitHead(folder)!==proof.head)return;
        const current=(JSON.parse(snapshot).projects??[]).find(p=>p.id===project.id);
        if(!current||latestGitTips(current).join()!==tips.join())return;
        if(cache)for(const range of redundantGitRanges(ranges,proof.head,proof.ancestors??[],keepGitBundles()))if(await old(range.payload)){
          await cache.removeEntry(range.payload.name);await cache.removeEntry(range.metadata);count.bundles++;
        }
        // An outbox job remains protected until superseded. This includes offline jobs.
        const job=await readRecord('inventory','git-outbox:'+project.id);
        const protectedNow=new Set([...protectedUploads,job?.file,job?.deltaFile]);
        if(outbox&&!keepGitBundles())for await(const entry of outbox.values())if(entry.kind==='file'&&entry.name.startsWith(project.id+'-')&&entry.name.endsWith('.bundle')&&!protectedNow.has(entry.name)&&await old(entry)){
          await outbox.removeEntry(entry.name);count.uploads++;
        }
      });
    }catch(error){console.warn('Git cache cleanup deferred:',error);}
  }
  const snapshots=await optional(root,'git-kicad-snapshots');
  if(snapshots)for await(const entry of snapshots.values())if(entry.kind==='directory'&&!protectedFolders.has(entry.name)){
    const links=[...records].filter(([key,value])=>key.startsWith('git-snapshot:')&&value?.folder===entry.name);
    let modified=Math.max(0,...links.map(([,value])=>value.created_at??0));
    if(!modified){async function newest(dir){for await(const child of dir.values())if(child.kind==='file')modified=Math.max(modified,(await child.getFile()).lastModified);else await newest(child);}await newest(entry);}
    if(Date.now()-modified<minAge)continue;
    // Recheck active bindings immediately before deletion; folder handles may have changed.
    if((JSON.parse(snapshot).projects??[]).some(p=>p.kicad?.browser_folder===entry.name))continue;
    await snapshots.removeEntry(entry.name,{recursive:true});
    for(const [key]of links)await deleteGitCacheRecord('inventory',key);
    await deleteGitCacheRecord('folders',entry.name);count.snapshots++;
  }
  return count;
}
async function gitCacheRecords(){
  const db=await database();const keys=await new Promise((resolve,reject)=>{const r=db.transaction('inventory').objectStore('inventory').getAllKeys();r.onsuccess=()=>resolve(r.result);r.onerror=()=>reject(r.error);});
  const result=new Map();for(const key of keys)if(typeof key==='string'&&(key.startsWith('git-snapshot:')||key.startsWith('git-outbox:')))result.set(key,await readRecord('inventory',key));return result;
}
async function deleteGitCacheRecord(store,key){const db=await database();await new Promise((resolve,reject)=>{const tx=db.transaction(store,'readwrite');tx.objectStore(store).delete(key);tx.oncomplete=resolve;tx.onerror=()=>reject(tx.error);});}

async function folderGitHead(folder) {
  const root=await readRecord('folders',folder);
  const read=async path=>{try{let directory=root;const parts=path.split('/');for(const part of parts.slice(0,-1))directory=await directory.getDirectoryHandle(part);return await(await(await directory.getFileHandle(parts.at(-1))).getFile()).text();}catch(error){if(error.name==='NotFoundError')return null;throw error;}};
  const head=(await read('.git/HEAD'))?.trim();
  if(!head?.startsWith('ref: '))return head;
  const reference=head.slice(5);
  const loose=(await read('.git/'+reference))?.trim();
  if(loose)return loose;
  return (await read('.git/packed-refs'))?.split('\n').find(line=>line.endsWith(' '+reference))?.split(' ')[0]??null;
}

async function applyGitSnapshot(projectId, tip, selection) {
  // Serialize with UI saves and mesh adoption. The worker rechecks the latest tip
  // after download, so a stale bundle or newly divergent history cannot replace it.
  syncBusy++;
  const job = saveQueue.then(() => withGitProcessing(async () => {
    if (resetting || storageFailure) return;
    // Older/manual snapshot attachment used a random revision, indistinguishable
    // from a physical folder in Rust. Verify its OPFS provenance before allowing
    // Rust to migrate that binding to automatic updates.
    selection = {...selection};
    const old = JSON.parse(snapshot).projects?.find(p => p.id === projectId)?.kicad;
    if (old?.browser_folder) {
      const handle = await readRecord('folders', old.browser_folder);
      try {
        const root = await navigator.storage.getDirectory();
        const snapshots = await root.getDirectoryHandle('git-kicad-snapshots');
        const internal = await snapshots.getDirectoryHandle(old.browser_folder);
        if (handle?.isSameEntry && await handle.isSameEntry(internal))
          selection.git_managed_folder = old.browser_folder;
      } catch (error) {
        if (error.name !== 'NotFoundError') throw error;
      }
    }
    const alreadyApplied=old?.revision==='git-snapshot:'+projectId+':'+tip;
    if(alreadyApplied&&selection.git_managed_folder&&old.browser_folder===selection.folder){gitUpdate(projectId,'Applied',tip);return;}
    if(old?.browser_folder&&!selection.git_managed_folder){
      // Legacy bindings did not record an applied tip. Read their actual HEAD;
      // Rust still requires a clean tree and ancestry before any fast-forward.
      const expected=await folderGitHead(old.browser_folder);
      if(alreadyApplied&&expected===tip){gitUpdate(projectId,'Applied',tip);return;}
      if(!expected)throw Error('The linked folder has no complete Git checkout. Choose it again in Edit project to finish copying the repository.');
      if(expected){
        const checked=await runLocalGit(old.browser_folder,projectId,'checkout','','','',{tip,checkout_head:expected});
        selection={folder:old.browser_folder,git_managed_folder:old.browser_folder,choices:checked.choices,files:{},managed_revision:'git-snapshot:'+projectId+':'+tip};
      }
    }
    gitUpdate(projectId,'Applying',tip);
    const state = await sqliteStorage.call('apply_git_snapshot', {projectId, tip, selection});
    sqliteStorage.state = state;
    adoptSyncState(state);
    const applied=JSON.parse(state.snapshot).projects?.find(project=>project.id===projectId)?.kicad;
    if(applied?.revision!=='git-snapshot:'+projectId+':'+tip)
      throw Error('Git '+tip.slice(0,10)+' received but not applied. Check Sync for pending comparisons or competing repository tips.');
    gitUpdate(projectId,'Applied',tip);
  })).finally(() => { syncBusy--; });
  saveQueue = job.catch(() => {});
  return job;
}

export function pickGitBundles(projectId) {
  return new Promise((resolve, reject) => {
    const input = document.createElement('input');
    input.type = 'file'; input.accept = '.bundle'; input.multiple = true; input.hidden = true;
    input.oncancel = () => { input.remove(); resolve(null); };
    input.onchange = () => {
      const files = Array.from(input.files || []);
      input.remove();
      if (!files.length) resolve(null);
      else importGitBundles(files, projectId).then(resolve,reject);
    };
    document.body.append(input); input.click();
  });
}

export function pickKiCadFolder() {
  if(typeof window.showDirectoryPicker==='function')return chooseLinkedKiCadFolder().catch(error=>{if(error.name==='AbortError')return null;throw error;});
  return new Promise((resolve, reject) => {
    const input = document.createElement('input');
    input.type='file'; input.webkitdirectory=true; input.multiple=true; input.hidden=true;
    input.oncancel=()=>{input.remove();resolve(null);};
    input.onchange=async()=>{
      try {
        const selected=Array.from(input.files||[]).filter(file=>/\.kicad_(pro|sch|pcb|dru)$/i.test(file.name));
        if(selected.length>512 || selected.reduce((n,f)=>n+f.size,0)>64*1024*1024) throw Error('The browser preview accepts up to 512 KiCad files totalling 64 MB. Larger projects can be linked in the desktop app.');
        const files={};
        for(const file of selected) files[file.webkitRelativePath||file.name]=await file.text();
        if(!Object.keys(files).some(name=>name.endsWith('.kicad_pro'))) throw Error('Choose the folder containing your .kicad_pro project and schematic files.');
        input.remove();resolve(JSON.stringify({folder:null,choices:Object.keys(files).filter(name=>name.endsWith('.kicad_pro')).sort(),files}));
      } catch(error) {input.remove();reject(error);}
    };
    document.body.append(input);input.click();
  });
}

let notesOwner = null;
let notesPasteInstalled = false;
export function setNotesOwner(owner) { notesOwner = owner || null; }
export function installNotesPaste(onImage, onError) {
  if (notesPasteInstalled) return;
  notesPasteInstalled = true;
  // Capture before egui's text paste handler. Keep the owner at the time of paste.
  document.addEventListener('paste', async event => {
    const owner = notesOwner;
    if (!owner) return;
    const file = Array.from(event.clipboardData?.items || [])
      .find(item => item.kind === 'file' && item.type.startsWith('image/'))?.getAsFile();
    if (!file) return; // Ordinary text paste continues to egui.
    event.preventDefault(); event.stopImmediatePropagation();
    try {
      if (file.size > 30_000_000) throw Error('Clipboard image exceeds 30 MB.');
      onImage(owner, new Uint8Array(await file.arrayBuffer()));
    } catch (error) { onError(String(error)); }
  }, true);
}
export async function readNotesClipboard() {
  if (!navigator.clipboard?.read) throw Error('Use Ctrl/Cmd+V to paste an image in this browser.');
  const items = await navigator.clipboard.read();
  for (const item of items) {
    const type = item.types.find(type => type.startsWith('image/'));
    if (!type) continue;
    const blob = await item.getType(type);
    if (blob.size > 30_000_000) throw Error('Clipboard image exceeds 30 MB.');
    return new Uint8Array(await blob.arrayBuffer());
  }
  throw Error('Copy an image to the clipboard first.');
}
// Durable storage is separate from the small browser preference/localStorage area.
const inventoryKey = 'bom-inventory.web.inventory.v1';
let dbPromise;
function database() {
  return dbPromise ||= new Promise((resolve,reject)=>{
    const request=indexedDB.open('bom-inventory',1);
    request.onupgradeneeded=()=>{const db=request.result;db.createObjectStore('inventory');db.createObjectStore('folders');};
    request.onsuccess=()=>resolve(request.result);
    request.onerror=()=>reject(request.error);
    request.onblocked=()=>reject(Error('Close other inventory tabs to upgrade browser storage.'));
  });
}
async function readRecord(store,key) {
  const db=await database();
  return new Promise((resolve,reject)=>{const tx=db.transaction(store,'readonly');const request=tx.objectStore(store).get(key);request.onsuccess=()=>resolve(request.result);request.onerror=()=>reject(request.error);});
}
async function putRecord(store,key,value) {
  const db=await database();
  return new Promise((resolve,reject)=>{const tx=db.transaction(store,'readwrite');tx.objectStore(store).put(value,key);tx.oncomplete=()=>resolve();tx.onabort=()=>reject(tx.error||Error('Browser storage write failed'));tx.onerror=()=>{};});
}
let snapshot=null, durable=null, saveQueue=Promise.resolve(), storageFailure=null, pendingSaves=0, resetting=false;
let workspaceStatus = '';
function persistenceStatus(message) {
  workspaceStatus = message;
}
export function inventoryWorkspaceStatus() {
  return globalThis.__inventorySqliteBridge?.workspaceStatus() ?? workspaceStatus;
}
let sqliteStorage, syncObserver=null, syncRefresh=false, syncDirty=false, syncBusy=0;
export function isBrowserDemo() {
  return document.querySelector('meta[name="vispace-demo"]')?.content === 'enabled';
}
export async function initInventoryStorage() {
  if(globalThis.__inventorySqliteBridge)return globalThis.__inventorySqliteBridge.snapshot();
  const {InventoryStorage}=await import(new URL('./storage-client.js',document.baseURI));
  sqliteStorage=new InventoryStorage();
  const legacy=(await readRecord('inventory','current'))??globalThis.localStorage?.getItem(inventoryKey)??null;
  const folder=await readRecord('folders','inventory-workspace');
  const state=await sqliteStorage.init(legacy,folder,isBrowserDemo());
  snapshot=durable=state.snapshot;
  globalThis.__inventorySqliteBridge={identity:()=>sqliteStorage.state.applicationId, busy:inventorySaveBusy,pairingPending:inventoryPairingPending,workspaceStatus:()=>workspaceStatus,gitDiagnostics,snapshot:()=>snapshot,save:saveInventory,choose:chooseInventoryWorkspace,reconnect:reconnectInventoryWorkspace,backup:backupInventorySqlite,reset:resetBrowserInventory,sync:inventorySync,observe:installInventorySyncObserver,graph:inventoryNetworkGraph,head:inventoryLocalHead,history:inventoryHistoryPage};
  sqliteStorage.onchange=()=>{refreshSync();saveQueue.then(()=>pollGitBundles());};
  setInterval(pollGitBundles, 8000);
  if(typeof window!=='undefined')window.addEventListener('beforeunload',event=>{if(pendingSaves||(storageFailure&&snapshot!==durable)){event.preventDefault();event.returnValue='';}});
  persistenceStatus(state.needsPermission?'Choose the workspace folder again in Preferences → General to enable saving':state.workspace?`SQLite · ${state.workspace}`:'SQLite · browser storage');
  return snapshot;
}
export function inventorySnapshot(){return sqliteStorage?snapshot:globalThis.__inventorySqliteBridge?.snapshot()??snapshot;}
// Background reconciliation retries after sync adoption instead of surfacing a
// transient conflict as an inventory error. Consult the canonical module bridge.
export function inventorySaveBusy(){return sqliteStorage?Boolean(syncBusy||pendingSaves):Boolean(globalThis.__inventorySqliteBridge?.busy?.());}
export function saveInventory(next,expected) {
  if(resetting)throw Error('The browser inventory is being reset.');
  if(!sqliteStorage&&globalThis.__inventorySqliteBridge)return globalThis.__inventorySqliteBridge.save(next,expected);
  if(sqliteStorage?.state.sync?.reviews.length)throw Error('Resolve the pending comparisons in Sync before editing.');
  if(storageFailure)throw Error(`Saving is paused. Reconnect the workspace or export your changes. ${storageFailure}`);
  if(syncBusy)throw Error('Sync is applying received changes. Please try again in a moment.');
  if(pendingSaves)throw Error('The previous change is still saving. Please wait a moment.');
  if(snapshot!==(expected??null))throw Error('Inventory changed during saving. Reload before trying again.');
  if(sqliteStorage?.state.needsPermission)throw Error('Choose the workspace folder again in Preferences → General to enable saving.');
  const expectedRevision=sqliteStorage.state.revision;
  snapshot=next;pendingSaves++;persistenceStatus('Saving files and SQLite…');
  const job=saveQueue.then(()=>sqliteStorage.save(next,expectedRevision)).then(state=>{
    snapshot=durable=state.snapshot;
    persistenceStatus(state.workspace?`Saved · ${state.workspace}`:'Saved in browser SQLite');
    return snapshot;
  }).catch(error=>{
    if(error.message?.startsWith('Shared save:')){
      snapshot=durable;syncObserver?.(durable);persistenceStatus(error.message);refreshSync();return durable;
    }
    storageFailure=error;persistenceStatus(`Saving paused: ${error.message||error}`);throw error;}).finally(()=>{pendingSaves--;});
  saveQueue=job.catch(()=>{});return job;
}
export async function chooseInventoryWorkspace(){
  if(!sqliteStorage&&globalThis.__inventorySqliteBridge)return globalThis.__inventorySqliteBridge.choose();
  if(!globalThis.showDirectoryPicker)throw Error('Directory access is unavailable here. This browser will keep SQLite, files and chunks in browser storage.');
  // Start during the click so browsers can display the permission picker.
  const folder=await showDirectoryPicker({mode:'readwrite',id:'inventory-workspace'});
  await saveQueue;await sqliteStorage.bind(folder);
  await putRecord('folders','inventory-workspace',folder);
  // Preserve a failed in-memory edit before reopening the newly authorized workspace.
  if(storageFailure&&snapshot!==durable)await sqliteStorage.save(snapshot);
  storageFailure=null;return sqliteStorage.state;
}
export async function selectInventoryWorkspace() {
  try {
    await chooseInventoryWorkspace();
    location.reload();
  } catch (error) {
    if (error.name !== 'AbortError') throw error;
  }
}

export async function reconnectInventoryWorkspace(){
  if(!sqliteStorage&&globalThis.__inventorySqliteBridge)return globalThis.__inventorySqliteBridge.reconnect();
  const folder=await readRecord('folders','inventory-workspace');
  if(!folder)return chooseInventoryWorkspace();
  if(await folder.requestPermission({mode:'readwrite'})!=='granted')throw Error('Workspace permission was not granted.');
  await saveQueue;await sqliteStorage.bind(folder);
  if(storageFailure&&snapshot!==durable)await sqliteStorage.save(snapshot);
  storageFailure=null;return sqliteStorage.state;
}
export async function resetBrowserInventory(){
  if(!sqliteStorage&&globalThis.__inventorySqliteBridge)return globalThis.__inventorySqliteBridge.reset();
  if(!sqliteStorage||resetting)return;
  const prompt = isBrowserDemo()
    ? 'Reset the demo? This clears this browser’s demo inventory and restores the sample data. Linked workspace files are kept.'
    : 'Reset this browser inventory? Its database, attachments, and action history will be permanently deleted. Any linked workspace folder will be disconnected and its files will be kept.';
  if(!window.confirm(prompt))return;
  resetting=true;
  try {
    await saveQueue;
    const db=await database();
    await new Promise((resolve,reject)=>{
      const tx=db.transaction(['inventory','folders'],'readwrite');
      tx.objectStore('inventory').delete('current');
      tx.objectStore('folders').delete('inventory-workspace');
      tx.oncomplete=resolve;
      tx.onabort=()=>reject(tx.error||Error('Could not clear legacy inventory data.'));
      tx.onerror=()=>{};
    });
    localStorage.removeItem(inventoryKey);
    await sqliteStorage.call('reset_inventory');
    sqliteStorage.worker.terminate();
    snapshot=durable=null;
    storageFailure=null;
    window.location.reload();
  } catch(error) {
    resetting=false;
    throw error;
  }
}

export async function backupInventorySqlite(){if(!sqliteStorage&&globalThis.__inventorySqliteBridge)return globalThis.__inventorySqliteBridge.backup();await flushInventory();return sqliteStorage.backup();}

export async function inventoryHistoryPage(before, limit) {
 if (!sqliteStorage && globalThis.__inventorySqliteBridge) {
  return globalThis.__inventorySqliteBridge.history(before, limit);
 }
 const page = await sqliteStorage.call('history', {before: JSON.parse(before), limit});
 return JSON.stringify(page);
}
export function inventoryLocalHead(){if(!sqliteStorage&&globalThis.__inventorySqliteBridge)return globalThis.__inventorySqliteBridge.head();return sqliteStorage?.state.sync?.appliedHead??'';}
export function inventoryNetworkGraph(){if(!sqliteStorage&&globalThis.__inventorySqliteBridge)return globalThis.__inventorySqliteBridge.graph();return JSON.stringify(sqliteStorage?.state.sync?.topology??{local:'',nodes:[],edges:[]});}
export function installInventorySyncObserver(observer){if(!sqliteStorage&&globalThis.__inventorySqliteBridge)return globalThis.__inventorySqliteBridge.observe(observer);syncObserver=observer;}
function pairingPending(state) {
 return state?.sync?.peers.some(peer => peer.active && peer.pairingId && !peer.peerId) ?? false;
}
export function inventoryPairingPending() {
 if (!sqliteStorage) return globalThis.__inventorySqliteBridge?.pairingPending() ?? false;
 return pairingPending(sqliteStorage.state);
}
let observedPairing = false;
let observedHead = '';
let observedNodeCount = 1;
let observedLeader = null;
function adoptSyncState(state) {
 if (storageFailure) throw Error('Saving is paused. Reconnect the workspace before applying received changes.');
 const changed = snapshot !== state.snapshot;
 snapshot = durable = state.snapshot;
 const head = state.sync?.appliedHead ?? '';
 const nodeCount = state.sync?.topology?.nodes.filter(node => node.reachable).length ?? 1;
 const leader=state.sync?.topology?.leader??null;
 const pairing = pairingPending(state);
 if (changed || head !== observedHead || nodeCount !== observedNodeCount || leader !== observedLeader || pairing !== observedPairing) syncObserver?.(snapshot);
 observedPairing = pairing;
 observedLeader=leader;
 observedHead = head;
 observedNodeCount = nodeCount;
 return state.sync;
}
function refreshSync(){if(resetting)return;syncDirty=true;if(syncRefresh)return;syncRefresh=true;syncBusy++;const job=saveQueue.then(async()=>{do{syncDirty=false;const state=await sqliteStorage.sync('state');adoptSyncState(state);globalThis.dispatchEvent?.(new Event('inventory-sync-changed'));}while(syncDirty);}).catch(error=>persistenceStatus(`Sync: ${error.message||error}`)).finally(()=>{syncRefresh=false;syncBusy--;});saveQueue=job;}
export function openSyncDialog() {
  globalThis.dispatchEvent(new Event('inventory-sync-open'));
}
export async function inventorySync(op='state',args={}){
 if(resetting)throw Error('The browser inventory is being reset.');
 if(!sqliteStorage&&globalThis.__inventorySqliteBridge)return globalThis.__inventorySqliteBridge.sync(op,args);
 if(op!=='draft')syncBusy++;
 const job=saveQueue.then(async()=>{if(storageFailure)throw Error('Reconnect the workspace before syncing.');if(op==='draft')return sqliteStorage.call(op,args);return adoptSyncState(await sqliteStorage.sync(op,args));}).finally(()=>{if(op!=='draft')syncBusy--;});saveQueue=job.catch(()=>{});return job;
}

const maxKiCadBytes=64*1024*1024;
async function folderFiles(handle, includeContents, mode) {
  const files={};let count=0,total=0;
  async function walk(directory,prefix,depth){
    if(depth>32)throw Error('KiCad folder nesting exceeds 32 levels.');
    for await(const [name,entry] of directory.entries()){
      if(entry.kind==='directory') { if(!name.startsWith('.')&&!name.endsWith('-backups'))await walk(entry,prefix+name+'/',depth+1); }
      else if(/\.kicad_(pro|sch|pcb|dru)$/i.test(name)) {
        if(++count>512)throw Error('Choose a project folder with at most 512 KiCad files.');
        const path=prefix+name;
        if(includeContents && !(mode==='schematics' && /\.kicad_(pcb|dru)$/i.test(name)) && !(mode==='board' && /\.kicad_sch$/i.test(name))){const file=await entry.getFile();total+=file.size;if(file.size>32*1024*1024||total>maxKiCadBytes)throw Error('KiCad files exceed the 64 MB project limit.');files[path]=await file.text();}
        else files[path]='';
      }
    }
  }
  await walk(handle,'',0);return files;
}
export async function readKiCadFolder(folder, mode) {
  const handle=await readRecord('folders',folder);
  if(!handle)throw Error('Folder link is unavailable. Edit this project and choose its KiCad folder again.');
  if(handle.queryPermission && await handle.queryPermission({mode:'read'})!=='granted')throw Error('Folder access needs permission. Use Allow folder access or choose the folder again.');
  return folderFiles(handle,true,mode);
}
export async function authorizeKiCadFolder(folder) {
  const handle=await readRecord('folders',folder);
  if(!handle)throw Error('Choose the KiCad folder again in Edit project.');
  if(await handle.requestPermission({mode:'read'})!=='granted')throw Error('Read access to the KiCad folder was not granted.');
}
export async function pickManagedKiCadFolder(projectId,expectedTip){
  let handle;try{handle=await showDirectoryPicker({mode:'readwrite'});}catch(error){if(error.name==='AbortError')return null;throw error;}
  const folder=crypto.randomUUID();await putRecord('folders',folder,handle);
  try{
    let empty=true;for await(const _ of handle.values()){empty=false;break;}
    let recover=false;
    if(!empty){const review=await runLocalGit(folder,projectId,'log','','','');recover=review.initialized&&!review.head;if(!review.initialized||(!recover&&review.head!==expectedTip))throw Error('Choose an empty folder or a Git repository whose HEAD matches the attached project.');}
    let result=await runLocalGit(folder,projectId,'checkout','','','',{tip:expectedTip,checkout_head:empty||recover?null:expectedTip,recover_checkout:recover});
    if(recover&&result.selected_tip!==expectedTip)result=await runLocalGit(folder,projectId,'checkout','','','',{tip:expectedTip,checkout_head:result.selected_tip});
    const head=await folderGitHead(folder);
    const git=await handle.getDirectoryHandle('.git');await git.getFileHandle('index');
    if(head!==expectedTip)throw Error('Repository copy is incomplete: the destination HEAD was not updated.');
    return JSON.stringify({folder,choices:result.choices,files:{},git_managed_folder:folder,managed_revision:'git-snapshot:'+projectId+':'+expectedTip});
  }catch(error){await putRecord('folders',folder,null);throw error;}
}
export async function chooseLinkedKiCadFolder() {
  const handle=await window.showDirectoryPicker({mode:'readwrite'});
  const entries=await folderFiles(handle,false);
  const choices=Object.keys(entries).filter(n=>n.endsWith('.kicad_pro')).sort();
  if(!choices.length)throw Error('Choose the folder containing your .kicad_pro and schematic files.');
  const folder=crypto.randomUUID();
  await putRecord('folders',folder,handle);
  return JSON.stringify({folder,choices,files:{}});
}

let watchedFolder=null, watchGeneration=0, watchCallback=null, watchTimer=null;
const kiCadWatchStamps=new Map();
export function installKiCadWatch(callback){watchCallback=callback;}
export function setKiCadWatch(folder) {
  folder=folder||null;
  if(folder===watchedFolder)return;
  watchedFolder=folder;const generation=++watchGeneration;
  clearTimeout(watchTimer);
  if(!folder)return;
  let previous=kiCadWatchStamps.get(folder)??null;
  const check=async()=>{
    try {
      const handle=await readRecord('folders',folder);
      if(!handle||await handle.queryPermission({mode:'read'})!=='granted')return;
      const entries=[];let count=0;
      async function scan(dir,prefix,depth){
        if(depth>32)throw Error('Folder nesting exceeds 32 levels');
        for await(const [name,entry] of dir.entries()){
          if(entry.kind==='directory'){if(!name.startsWith('.')&&!name.endsWith('-backups'))await scan(entry,prefix+name+'/',depth+1);}
          else if(/\.kicad_(pro|sch|pcb|dru)$/i.test(name)){
            if(++count>512)throw Error('Too many KiCad files');
            const file=await entry.getFile();entries.push([prefix+name,file.lastModified,file.size]);
          }
        }
      }
      await scan(handle,'',0);
      const stamp=JSON.stringify(entries.sort((a,b)=>a[0].localeCompare(b[0])));
      if(generation!==watchGeneration)return;
      if(previous!==null&&stamp!==previous)watchCallback?.(folder);
      previous=stamp;
      kiCadWatchStamps.set(folder,stamp);
    }catch{/* A refresh surfaces file errors without repeated watcher alerts. */}
    finally{if(generation===watchGeneration)watchTimer=setTimeout(check,2000);}
  };
  void check();
}

export async function flushInventory(){await saveQueue;if(storageFailure)throw storageFailure;}
export function installSupplierBridge(request) {
  document.documentElement.dataset.bomInventoryBridge='1';
  window.addEventListener('message',async event=>{
    const msg=event.data;
    if(event.source!==window||event.origin!==location.origin||msg?.channel!=='bom-inventory.supplier.v1'||msg.direction!=='request')return;
    if(typeof msg.id!=='string'||!['health','projects','purchase'].includes(msg.operation)||typeof msg.payload!=='string'||msg.payload.length>1_000_000)return;
    const response={channel:msg.channel,direction:'response',id:msg.id};
    try { response.value=JSON.parse(await request(msg.operation,msg.payload)); }
    catch(error){response.error=String(error);delete response.value;}
    window.postMessage(response,location.origin);
  });
}

// The browser reads only explicitly granted project/model folders, never native paths.
export async function chooseModelsFolder() {
  if(typeof window.showDirectoryPicker==='function') {
    try {const handle=await window.showDirectoryPicker({mode:'read'});const folder=crypto.randomUUID();await putRecord('folders',folder,handle);return folder;}
    catch(error){if(error.name==='AbortError')return null;throw error;}
  }
  return new Promise((resolve,reject)=>{
    const input=document.createElement('input');input.type='file';input.webkitdirectory=true;input.multiple=true;input.hidden=true;
    input.oncancel=()=>{input.remove();resolve(null);};
    input.onchange=async()=>{
      try {
        const modelFiles={};let total=0,count=0;
        for(const file of input.files||[]) {
          if(!/\.(step|stp|wrl)$/i.test(file.name))continue;
          total+=file.size;if(file.size>32*1024*1024||total>128*1024*1024||++count>5000)throw Error('Choose a model subset smaller than 128 MB. Browser folder linking is available in Chrome and Edge.');
          const path=file.webkitRelativePath.split('/').slice(1).join('/')||file.name;modelFiles[path]=file;
        }
        const folder=crypto.randomUUID();await putRecord('folders',folder,{modelFiles});input.remove();resolve(folder);
      }catch(error){input.remove();reject(error);}
    };
    document.body.append(input);input.click();
  });
}
export async function readBoardModels(board,projectPath,projectFolder,modelsFolder,parseLibraries,demo=false) {
  const project=projectFolder?await readRecord('folders',projectFolder):null;
  const library=modelsFolder?await readRecord('folders',modelsFolder):null;
  let hosted={};
  if(demo){
    try {
      const response=await fetch(new URL('./demo/models/manifest.json',import.meta.url));
      if(response.ok){
        const manifest=await response.json();
        if(manifest.project===projectPath)hosted=manifest.files??{};
      }
    }catch{/* A demo without bundled models can still render the board. */}
  }
  return loadBoardModelsFromHandles(board,projectPath,project,library,parseLibraries,hosted);
}
export async function loadBoardModelsFromHandles(board,projectPath,project,library,parseLibraries,hosted={}) {
  const base=projectPath.replaceAll('\\','/').split('/').slice(0,-1).join('/');
  const paths=new Set(board.footprint_list.flatMap(f=>(f.models||[]).filter(m=>!m.hidden).map(m=>m.path)));
  const assets={};let total=0;
  async function read(root,path){
    if(!root)return null;
    if(!root.modelFiles&&await root.queryPermission({mode:'read'})!=='granted')return null;
    const parts=[];
    for(const part of path.replaceAll('\\','/').split('/')){
      if(!part||part==='.')continue;
      if(part==='..'){if(!parts.length)return null;parts.pop();}else parts.push(part);
    }
    if(!parts.length)return null;
    if(root.modelFiles)return root.modelFiles[parts.join('/')]||null;
    let directory=root;
    for(const name of parts.slice(0,-1))directory=await directory.getDirectoryHandle(name);
    return (await directory.getFileHandle(parts.at(-1))).getFile();
  }
  let footprintLibraries={};
  if(parseLibraries && project) {
    try {
      const file=await read(project,base?`${base}/fp-lib-table`:'fp-lib-table');
      if(file && file.size<=1024*1024)footprintLibraries=parseLibraries(await file.text());
    }catch{/* An absent library table does not prevent standard model resolution. */}
  }
  for(const original of paths){
    if(original.startsWith('kicad-embed://'))continue;
    let root=project||library,path=original.replaceAll('\\','/');
    if(/\$\{KICAD\d+_3DMODEL_DIR\}/.test(path)) {root=library;path=path.replace(/\$\{KICAD\d+_3DMODEL_DIR\}\/?/,'');}
    else if(path.includes('${KIPRJMOD}')) path=path.replace('${KIPRJMOD}',base);
    else if(path.startsWith('/')||/^[a-z]:/i.test(path)) {
      const index=path.search(/[^/]+\.3dshapes\//);if(index<0)continue;root=library;path=path.slice(index);
    } else path=base?`${base}/${path}`:path;
    if(path.includes('${'))continue;
    const locations=[{root,path}];
    if(!original.includes('${') && !original.startsWith('/') && !/^[a-z]:/i.test(original)) {
      for(const footprint of board.footprint_list) {
        if(!(footprint.models||[]).some(m=>m.path===original))continue;
        const uri=footprintLibraries[(footprint.library||'').split(':')[0]];
        if(!uri)continue;
        let directory=uri.replaceAll('\\','/'),folder=project;
        if(/\$\{KICAD\d+_3DMODEL_DIR\}/.test(directory)) {
          folder=library;directory=directory.replace(/\$\{KICAD\d+_3DMODEL_DIR\}\/?/,'');
        }else if(directory.includes('${KIPRJMOD}'))directory=directory.replace('${KIPRJMOD}',base);
        else if(directory.startsWith('/')||/^[a-z]:/i.test(directory))continue;
        else directory=base?`${base}/${directory}`:directory;
        if(directory.includes('${'))continue;
        locations.push({root:folder,path:`${directory}/${original}`});
      }
    }
    const alternatives=locations.flatMap(({root,path})=>{
      const paths=[path];
      if(/\.wrl$/i.test(path))paths.push(path.replace(/\.wrl$/i,'.step'),path.replace(/\.wrl$/i,'.stp'));
      else paths.push(path.replace(/\.(step|stp)$/i,'.wrl'));
      return paths.map(path=>({root,path}));
    });
    const bundled=Object.hasOwn(hosted,original)?hosted[original]:null;
    const segments=typeof bundled==='string'?bundled.split('/'):[];
    if(['cln17','kicad'].includes(segments[0])&&segments.every(part=>part&&part!=='.'&&part!=='..'&&!part.includes('\\'))){
      alternatives.push({path:bundled,url:new URL('./demo/models/'+segments.map(encodeURIComponent).join('/'),import.meta.url)});
    }
    for(const {root,path:candidate,url} of alternatives){
      try{
        let file;
        if(url){
          const response=await fetch(url);
          if(!response.ok)continue;
          file=await response.blob();
        }else file=await read(root,candidate);
        if(!file)continue;
        if(file.size>32*1024*1024||total+file.size>128*1024*1024)continue;
        const bytes=new Uint8Array(await file.arrayBuffer());let binary='';
        for(let i=0;i<bytes.length;i+=16384)binary+=String.fromCharCode(...bytes.subarray(i,i+16384));
        assets[original]=[candidate,btoa(binary)];total+=file.size;break;
      }catch{/* Missing models are omitted; the board remains usable. */}
    }
  }
  return assets;
}

const max3dCacheBytes=128*1024*1024;
const cache3dKey=(folder,path,section='scene')=>`${section==='scene'?'3d-cache':'pcb-cache'}:${folder||'upload'}:${path}`;
const cacheMagic='VINVPK01';
function cache3dParts(path) {
  const parts=[];
  for(const part of path.replaceAll('\\','/').split('/').slice(0,-1)) {
    if(!part||part==='.')continue;
    if(part==='..'){if(!parts.length)throw Error('Cache path leaves project folder');parts.pop();}
    else if(part.includes(':'))throw Error('Cache path must be relative');
    else parts.push(part);
  }
  return parts;
}
function cacheBase64(bytes) {
  let binary='';for(let i=0;i<bytes.length;i+=16384)binary+=String.fromCharCode(...bytes.subarray(i,i+16384));
  return btoa(binary);
}
function cacheRanges(header,total) {
  if(total>2*max3dCacheBytes+24)throw Error('Project cache exceeds size limit');
  if(String.fromCharCode(...header.subarray(0,8))!==cacheMagic)return {pcb:[0,0],scene:[0,total]};
  if(header.length<24)throw Error('Truncated project cache');
  const view=new DataView(header.buffer,header.byteOffset,header.byteLength);
  const a=Number(view.getBigUint64(8,true)),b=Number(view.getBigUint64(16,true));
  if(a>max3dCacheBytes||b>max3dCacheBytes||24+a+b!==total)throw Error('Invalid project cache lengths');
  return {pcb:[24,a],scene:[24+a,b]};
}
export function packProjectCache(pcb,scene) {
  if(pcb.length>max3dCacheBytes||scene.length>max3dCacheBytes)throw Error('Project cache section exceeds size limit');
  const bytes=new Uint8Array(24+pcb.length+scene.length),view=new DataView(bytes.buffer);
  bytes.set(new TextEncoder().encode(cacheMagic));view.setBigUint64(8,BigInt(pcb.length),true);view.setBigUint64(16,BigInt(scene.length),true);
  bytes.set(pcb,24);bytes.set(scene,24+pcb.length);return bytes;
}
export function projectCacheSection(bytes,section) {
  const [start,len]=cacheRanges(bytes.subarray(0,24),bytes.length)[section];
  if(len>max3dCacheBytes)throw Error('Project cache section exceeds size limit');
  return bytes.slice(start,start+len);
}
async function cacheDirectory(root,path,mode) {
  if(!root||root.modelFiles||await root.queryPermission({mode})!=='granted')return null;
  let directory=root;
  for(const part of cache3dParts(path))directory=await directory.getDirectoryHandle(part);
  return directory;
}
const projectCacheName = '.vspace.cache';
async function readCacheSectionFromHandle(root,path,section) {
  const directory=await cacheDirectory(root,path,'read');if(!directory)return null;
  const file=await (await directory.getFileHandle(projectCacheName)).getFile();
  const header=new Uint8Array(await file.slice(0,24).arrayBuffer());
  const [start,len]=cacheRanges(header,file.size)[section];
  if(len>max3dCacheBytes)throw Error('Project cache section exceeds size limit');
  return new Uint8Array(await file.slice(start,start+len).arrayBuffer());
}
export async function read3dCacheFromHandle(root,path) {return readCacheSectionFromHandle(root,path,'scene');}
async function writeCacheSectionToHandle(root,path,bytes,section) {
  if(bytes.length>max3dCacheBytes)throw Error('Project cache section exceeds size limit');
  const directory=await cacheDirectory(root,path,'readwrite');if(!directory)return false;
  let other=new Uint8Array();try{other=await readCacheSectionFromHandle(root,path,section==='pcb'?'scene':'pcb')||other;}catch{}
  const packed=section==='pcb'?packProjectCache(bytes,other):packProjectCache(other,bytes);
  const file=await directory.getFileHandle(projectCacheName,{create:true});const stream=await file.createWritable();
  try {await stream.write(packed);await stream.close();}catch(error){try{await stream.abort();}catch{}throw error;}
  return true;
}
export async function write3dCacheToHandle(root,path,bytes) {return writeCacheSectionToHandle(root,path,bytes,'scene');}
async function readProjectSection(folder,path,section) {
  try {
    const fallback=await readRecord('inventory',cache3dKey(folder,path,section));
    if(fallback instanceof Uint8Array&&fallback.length<=max3dCacheBytes)return cacheBase64(fallback);
    const root=folder?await readRecord('folders',folder):null;
    const bytes=await readCacheSectionFromHandle(root,path,section);
    return bytes?.length?cacheBase64(bytes):null;
  }catch{return null;}
}
async function saveProjectSection(folder,path,encoded,section) {
  if(encoded.length>Math.ceil(max3dCacheBytes/3)*4)throw Error('Project cache section exceeds size limit');
  const bytes=Uint8Array.from(atob(encoded),c=>c.charCodeAt(0));let written=false;
  try {const root=folder?await readRecord('folders',folder):null;written=await writeCacheSectionToHandle(root,path,bytes,section);}catch{}
  if(written){try{await putRecord('inventory',cache3dKey(folder,path,section),null);}catch{}return `Saved ${projectCacheName}`;}
  await putRecord('inventory',cache3dKey(folder,path,section),bytes);
  return 'Cache saved in browser (project folder is read-only)';
}
export async function readProject3dCache(folder,path) {return readProjectSection(folder,path,'scene');}
export async function saveProject3dCache(folder,path,encoded) {return saveProjectSection(folder,path,encoded,'scene');}
export async function saveProjectPcbCache(folder,path,encoded) {return saveProjectSection(folder,path,encoded,'pcb');}
export async function pcbSourceStamps(folder,path) {
  if(!folder)return [];
  const root=await readRecord('folders',folder);if(!root)return [];
  const directory=await cacheDirectory(root,path,'read');if(!directory)return [];
  const projectName=path.replaceAll('\\','/').split('/').at(-1),stem=projectName.replace(/\.kicad_pro$/,'');
  const stamps=[];
  for(const name of [projectName,`${stem}.kicad_pcb`,`${stem}.kicad_dru`]) {
    let file;
    try{file=await (await directory.getFileHandle(name)).getFile();}
    catch(error){if(name.endsWith('.kicad_dru')&&error.name==='NotFoundError'){stamps.push({path:name,size:0,modified:'missing',header:[]});continue;}throw error;}
    if(file.size>32*1024*1024)throw Error('KiCad file exceeds 32 MB');
    stamps.push({path:name,size:file.size,modified:(BigInt(file.lastModified)*1000000n).toString(),header:[...new Uint8Array(await file.slice(0,512).arrayBuffer())]});
  }
  return stamps;
}
export function parsedPcbCacheMatches(encoded,stamps,version) {
  try {
    if(!encoded||!stamps.length)return false;
    // Decode only the small metadata prefix, never the compressed board here.
    const prefix=Uint8Array.from(atob(encoded.slice(0,Math.min(encoded.length,90000))),c=>c.charCodeAt(0));
    if(String.fromCharCode(...prefix.subarray(0,8))!=='VINVPCB1')return false;
    const len=new DataView(prefix.buffer).getUint32(8,true);if(len>64*1024||12+len>prefix.length)return false;
    const meta=JSON.parse(new TextDecoder().decode(prefix.subarray(12,12+len)));
    return meta.version===version&&JSON.stringify(meta.sources)===JSON.stringify(stamps);
  }catch{return false;}
}
export async function readProjectPcbCache(folder,path,stamps,version) {
  const cache=await readProjectSection(folder,path,'pcb');
  return parsedPcbCacheMatches(cache,stamps,version)?cache:null;
}


// Browser filesystem adapter. Git object/index/bundle processing runs in Grit/WASM.
export async function runLocalGit(...args){
  return ['share','commit','init','cache'].includes(args[2])?withGitCreating(()=>runLocalGitInner(...args)):runLocalGitInner(...args);
}
async function runLocalGitInner(folder,projectId,operation,message,name,email,options={}){
  if(operation==='diagnostics')return gitDiagnostics(projectId,Number(message)||100);

  const directory=await readRecord('folders',folder);
  if(!directory?.getDirectoryHandle)throw Error('Link a local directory to use Git.');
  let snapshotFolder=false;
  try{const root=await navigator.storage.getDirectory();const internal=await(await root.getDirectoryHandle('git-kicad-snapshots')).getDirectoryHandle(folder);snapshotFolder=await directory.isSameEntry(internal);}catch(error){if(error.name!=='NotFoundError')throw error;}
  if(snapshotFolder&&operation==='status')return {initialized:false,read_only:true,changes:[]};
  if(snapshotFolder&&['init','commit'].includes(operation))throw Error('Choose a writable repository folder in Edit project before committing.');
  if(operation==='log'||operation==='reachable'||operation==='cache'){if(await directory.queryPermission({mode:'read'})!=='granted')throw Error('Reconnect the project folder to read its history.');}
  else if(await directory.queryPermission({mode:'readwrite'})!=='granted'&&await directory.requestPermission({mode:'readwrite'})!=='granted')throw Error('Read/write permission is required for local Git.');
  return navigator.locks.request('vinventory-local-git:'+folder,async()=>{
    const files={};let count=0;
    async function scan(dir,prefix,depth){
      if(depth>32)throw Error('Repository directory nesting exceeds 32 levels.');
      for await(const [entryName,entry]of dir.entries()){
        const path=prefix+entryName;
        if(entry.kind==='directory'){if(path.startsWith('.git/')&&!['.git/objects','.git/refs','.git/info'].some(p=>path===p||path.startsWith(p+'/')))continue;await scan(entry,path+'/',depth+1);}
        else{if(++count>100000)throw Error('Repository exceeds the file metadata budget.');files[path]=await entry.getFile();}
      }
    }
    await scan(directory,'',0);const initialized=!!files['.git/HEAD'];
    if(['status','share','log','reachable','cache'].includes(operation)&&!initialized)return {initialized:false,changes:[],tip:null};
    const imports=[];
    const cached=[];
    if(operation==='import'||operation==='checkout'){
      const root=await navigator.storage.getDirectory();let cache;
      try{cache=await(await root.getDirectoryHandle('git-bundles')).getDirectoryHandle(projectId);}catch(error){if(error.name!=='NotFoundError')throw error;}
      if(cache)for await(const entry of cache.values())if(entry.kind==='file'&&entry.name.endsWith('.bundle')){
        const key='\0received/'+entry.name;files[key]=await entry.getFile();
        if(operation==='import')imports.push(key);else{
          let range;try{range=JSON.parse(await(await(await cache.getFileHandle(entry.name.replace(/\.bundle$/,'.json'))).getFile()).text());}catch{}
          if(range?.tips?.includes(options.tip)&&!imports.length)imports.push(key);else cached.push(key);
        }
      }
      if(!imports.length)throw Error('The required Git bundle is not available yet. Wait for sync and try again.');
    }
    const advertised=Object.values(JSON.parse(inventorySnapshot()).projects?.find(p=>p.id===projectId)?.git_tips??{}),known_tips=[...new Set(advertised.flatMap(t=>[t.commit,...(t.includes??[])]))];
    const result=await streamGit({...options,files,imports,cached,known_tips:options.known_tips??known_tips,operation:operation==='cache'?'share':operation,message,name,email,timestamp:Math.floor(Date.now()/1000)});
    try{
      const value=result.value;
      if(value.bundle?.length>maxGitTransferBytes)throw Error('Git bundle exceeds the 4 GiB transport limit');
      if(operation==='status'||operation==='log'||operation==='reachable')return {...value,io:result.stats};
      if(operation==='cache'){
        if(!value.tip||!value.bundle)throw Error('Repository has no committed history to cache');
        const root=await navigator.storage.getDirectory(),cache=await(await root.getDirectoryHandle('git-bundles',{create:true})).getDirectoryHandle(projectId,{create:true});
        await keepRawGitBundle(cache,gitRange(result.stage,value.bundle),[],[value.tip]);
        return {head:value.tip,ancestors:value.ancestors,initialized:true};
      }
      async function handle(path,create=false){const parts=path.split('/'),file=parts.pop();if(!file||parts.some(part=>!part||part==='.'||part==='..'))throw Error('Invalid repository path: '+JSON.stringify(path));let dir=directory;for(const part of parts)dir=await dir.getDirectoryHandle(part,{create});return [dir,file];}
      async function fileHandle(dir,name,options,path){try{return await dir.getFileHandle(name,options);}catch(error){if(error.name==='NotFoundError')throw error;throw Error('Cannot access repository file '+JSON.stringify(path)+': '+error.message);}}
      async function current(path){try{const[dir,file]=await handle(path);return new Uint8Array(await(await(await fileHandle(dir,file,undefined,path)).getFile()).arrayBuffer());}catch(error){if(error.name==='NotFoundError')return null;throw error;}}
      const expected=async path=>files[path]?new Uint8Array(await files[path].arrayBuffer()):undefined;
      const equal=(a,b)=>a===null?b===undefined:!!b&&a.length===b.length&&a.every((v,i)=>v===b[i]);
      for(const path of ['.git/HEAD','.git/index','.git/'+value.reference])if(!equal(await current(path),await expected(path)))throw Error('Repository changed during this operation. Review changes and retry.');
      const git=await directory.getDirectoryHandle('.git',{create:operation==='init'||operation==='checkout'});
      try{await git.getFileHandle('index.lock');throw Error('Repository is locked by another Git operation.');}catch(error){if(error.name!=='NotFoundError')throw error;}
      const lock=await git.getFileHandle('index.lock',{create:true});
      try{
        const branch='.git/'+value.reference,priority=path=>path.startsWith('.git/objects/')?0:path===branch?3:path==='.git/index'?2:1;
        for(const path of value.removes??[]){const[dir,file]=await handle(path);await dir.removeEntry(file);}
        for(const[path,range]of Object.entries(value.writes).sort(([a],[b])=>priority(a)-priority(b))){
          if(path===branch&&!equal(await current(path),await expected(path)))throw Error('Branch changed during commit. Retry after reviewing local Git.');
          const[dir,file]=await handle(path,true);await writeGitRange(await fileHandle(dir,file,{create:true},path),result.stage,range);
        }
      }finally{await git.removeEntry(lock.name);}
      if(value.tip){
        const root=await navigator.storage.getDirectory(),outbox=await root.getDirectoryHandle('git-outbox',{create:true}),payload=await immutableGitPayload(outbox,projectId+'-'+value.tip+'-'+value.bundle.sha256+'.bundle',result.stage,value.bundle);
        let deltaFile=null;
        if(value.incremental_bundle){const delta=await immutableGitPayload(outbox,projectId+'-'+value.tip+'-'+value.incremental_bundle.sha256+'.bundle',result.stage,value.incremental_bundle);deltaFile=delta.name;}
        const cache=await(await root.getDirectoryHandle('git-bundles',{create:true})).getDirectoryHandle(projectId,{create:true});
        await keepRawGitBundle(cache,await payload.getFile(),[],[value.tip]);
        await cacheGitSnapshot(projectId,value.tip,value,result.stage);localStorage.setItem('vinventory.git-base:'+projectId,value.tip);
        const previousOutbox=await readRecord('inventory','git-outbox:'+projectId);
        if(operation!=='share'||previousOutbox?.tip!==value.tip)await putRecord('inventory','git-outbox:'+projectId,{tip:value.tip,reference:value.reference,base:value.head,file:payload.name,deltaFile,sha256:value.bundle.sha256,deltaSha256:value.incremental_bundle?.sha256});
      }
      delete value.writes;delete value.bundle;delete value.incremental_bundle;delete value.snapshot;
      return {...value,io:result.stats,publisher:sqliteStorage?.state.applicationId??globalThis.__inventorySqliteBridge?.identity?.()};
    }finally{await removeGitJournal(result.temporary);}
  });
}

const gitUploadRetries=new Map();
async function uploadLocalGitBundles(peers) {
  if(!peers.length)return;
  const data=JSON.parse(snapshot);const root=await navigator.storage.getDirectory();let outbox;
  try{outbox=await root.getDirectoryHandle('git-outbox');}catch(error){if(error.name==='NotFoundError')return;throw error;}
  for(const project of data.projects??[]){
    const job=await readRecord('inventory','git-outbox:'+project.id);
    if(!job||!Object.values(project.git_tips??{}).some(t=>t.commit===job.tip))continue;
    for(const peer of peers){
      const key='git-uploaded:'+project.id+':'+peer.grant;
      const prior=await readRecord('inventory',key);
      if(prior===job.tip)continue;
      const retryKey=project.id+':'+peer.url,retry=gitUploadRetries.get(retryKey);
      if(retry?.tip===job.tip&&retry.after>Date.now())continue;
      const incremental=!!job.deltaFile&&prior===job.base;
      const file=await(await outbox.getFileHandle(incremental?job.deltaFile:job.file)).getFile();
      let sha256=incremental?job.deltaSha256:job.sha256;
      // Old releases reused a tip-based filename while keeping an earlier
      // checksum. Rehash those legacy payloads; new files are content-addressed
      // and are never overwritten by re-publication of the same commit.
      if(!sha256||!file.name.endsWith('-'+sha256+'.bundle')){
        const key='\0checksum',hashed=await streamGit({operation:'hash',files:{[key]:file},imports:[key]});
        try{sha256=hashed.value.checksum;}finally{await removeGitJournal(hashed.temporary);}
        const current=await readRecord('inventory','git-outbox:'+project.id);
        if(current?.tip===job.tip&&current.file===job.file){current[incremental?'deltaSha256':'sha256']=sha256;await putRecord('inventory','git-outbox:'+project.id,current);}
      }
      try{
        gitUpdate(project.id,'Uploading',job.tip);

        await new Promise((resolve,reject)=>{
          const socket=new WebSocket(new URL('/git-bundle',peer.url));let timer,acknowledged=0,creditWaiter=null;
          const reset=()=>{clearTimeout(timer);timer=setTimeout(()=>fail(Error('Bundle upload timed out')),60000);};
          const fail=error=>{clearTimeout(timer);socket.close();creditWaiter?.reject(error);creditWaiter=null;reject(error);};reset();
          socket.onopen=()=>socket.send(JSON.stringify({grant:peer.grant,project:project.id,tip:job.tip,base:incremental?job.base:null,flow:true,upload:{size:file.size,sha256}}));
          socket.onerror=()=>fail(Error('Bundle upload connection failed'));
          socket.onclose=()=>fail(Error('Bundle upload interrupted'));
          socket.onmessage=async({data})=>{try{
            const reply=JSON.parse(data);if(reply.error)throw Error(reply.error);
            if(reply.received!==undefined){if(!Number.isSafeInteger(reply.received)||reply.received<acknowledged||reply.received>file.size)throw Error('Invalid upload credit');acknowledged=reply.received;creditWaiter?.resolve();creditWaiter=null;reset();return;}
            if(reply.stored){clearTimeout(timer);socket.onclose=null;socket.close();resolve();return;}
            if(reply.upload){for(let offset=0;offset<file.size;offset+=64*1024){
              while(socket.bufferedAmount>1024*1024){if(socket.readyState!==1)throw Error('Bundle upload interrupted');await new Promise(r=>setTimeout(r,10));}
              while(offset-acknowledged>=1024*1024)await new Promise((resolve,reject)=>{creditWaiter={resolve,reject};});
              socket.send(await file.slice(offset,offset+64*1024).arrayBuffer());reset();
            }}
          }catch(error){fail(error);}};
        });
        await putRecord('inventory',key,job.tip);gitUploadRetries.delete(retryKey);gitUpdate(project.id,'Upload accepted',job.tip);
      }catch(error){
        if(incremental)await putRecord('inventory',key,null);
        const attempts=retry?.tip===job.tip?retry.attempts+1:1;
        gitUploadRetries.set(retryKey,{tip:job.tip,attempts,after:Date.now()+Math.min(30000,2000*2**Math.min(attempts-1,4))});
        gitUpdate(project.id,'Upload pending',String(error));console.warn('Local Git bundle upload will retry:',error);
      }
    }
  }
}

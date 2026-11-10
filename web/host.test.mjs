import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
const moduleSource = await readFile(new URL('./host.js', import.meta.url),'utf8');
const host = await import(`data:text/javascript;base64,${Buffer.from(moduleSource).toString('base64')}`);

test('image paste captures its note owner and leaves ordinary text paste alone', async () => {
  let handler;
  globalThis.document = { addEventListener(name,callback,capture) { assert.equal(name,'paste');assert.equal(capture,true);handler=callback; } };
  const received=[];
  host.installNotesPaste((owner,bytes)=>received.push([owner,Array.from(bytes)]),error=>assert.fail(error));
  host.setNotesOwner('note-a');
  let prevented=false;
  await handler({clipboardData:{items:[{kind:'string',type:'text/plain'}]},preventDefault(){prevented=true;},stopImmediatePropagation(){}});
  assert.equal(prevented,false);
  let finish;
  const pending=new Promise(resolve=>finish=resolve);
  const event={clipboardData:{items:[{kind:'file',type:'image/png',getAsFile:()=>({size:3,arrayBuffer:()=>pending})}]},preventDefault(){prevented=true;},stopImmediatePropagation(){}};
  const paste=handler(event);
  host.setNotesOwner('note-b');
  finish(new Uint8Array([1,2,3]).buffer);await paste;
  assert.equal(prevented,true);assert.deepEqual(received,[['note-a',[1,2,3]]]);
  host.setNotesOwner('');
  prevented=false;await handler(event);assert.equal(prevented,false);
});

test('clipboard button reads image bytes with a bounded payload', async () => {
  Object.defineProperty(globalThis,'navigator',{configurable:true,value:{clipboard:{read:async()=>[{types:['image/png'],getType:async()=>new Blob([new Uint8Array([4,5])],{type:'image/png'})}]}}});
  assert.deepEqual(Array.from(await host.readNotesClipboard()),[4,5]);
  navigator.clipboard.read=async()=>[{types:['text/plain']}];
  await assert.rejects(host.readNotesClipboard(),/Copy an image/);
});

test('3D models resolve granted library/project folders, extension fallback, and permissions', async()=>{
  const blob=text=>new Blob([text]);
  const library={modelFiles:{'Package.3dshapes/U.step':blob('ISO-10303-21;')}};
  const project={modelFiles:{'parts/C.wrl':blob('#VRML V2.0 utf8')}};
  const board={footprint_list:[{models:[
    {path:'${KICAD10_3DMODEL_DIR}/Package.3dshapes/U.wrl'},
    {path:'${KIPRJMOD}/parts/C.wrl'},
    {path:'${UNKNOWN}/secret.step'},
    {path:'../../outside.step'},
    {path:'kicad-embed://embedded.step'},
    {path:'hidden.step',hidden:true},
  ]}]};
  const assets=await host.loadBoardModelsFromHandles(board,'demo.kicad_pro',project,library);
  assert.equal(Object.keys(assets).length,2);
  assert.equal(atob(assets['${KICAD10_3DMODEL_DIR}/Package.3dshapes/U.wrl'][1]),'ISO-10303-21;');
  assert.equal(assets['${KICAD10_3DMODEL_DIR}/Package.3dshapes/U.wrl'][0],'Package.3dshapes/U.step');
  assert.equal(atob(assets['${KIPRJMOD}/parts/C.wrl'][1]),'#VRML V2.0 utf8');
  const denied={queryPermission:async()=>'denied'};
  assert.deepEqual(await host.loadBoardModelsFromHandles(board,'demo.kicad_pro',denied,denied),{});
});

test('bare STEP names resolve beside the configured footprint library',async()=>{
  const project={modelFiles:{
    'project/fp-lib-table':new Blob(['(fp_lib_table)']),
    'project/Lib/TPS25751DREFR/TPS25751DREFR.stp':new Blob(['ISO-10303-21;']),
  }};
  const board={footprint_list:[{library:'TPS25751DREFR:TPS25730ADREFR',models:[{path:'TPS25751DREFR.stp'}]}]};
  let parsed=false;
  const assets=await host.loadBoardModelsFromHandles(board,'project/USB_Hub.kicad_pro',project,null,text=>{
    assert.equal(text,'(fp_lib_table)');parsed=true;
    return {TPS25751DREFR:'${KIPRJMOD}/Lib/TPS25751DREFR'};
  });
  assert.equal(parsed,true);
  assert.equal(assets['TPS25751DREFR.stp'][0],'project/Lib/TPS25751DREFR/TPS25751DREFR.stp');
  assert.equal(atob(assets['TPS25751DREFR.stp'][1]),'ISO-10303-21;');
});

test('3D cache uses the project directory and never requests write permission in background',async()=>{
  const writes=[];let requested=false;
  const stream={write:async b=>writes.push([...b]),close:async()=>writes.push('closed')};
  const directory={getFileHandle:async(name,options)=>{
    assert.equal(name,'.vspace.cache');
    if(options){assert.equal(options.create,true);return {createWritable:async()=>stream};}
    return {getFile:async()=>new Blob([new Uint8Array([1,2,3])])};
  }};
  const root={queryPermission:async()=> 'granted',requestPermission:()=>{requested=true;throw Error('unexpected permission prompt');},getDirectoryHandle:async name=>{assert.equal(name,'nested');return directory;}};
  assert.deepEqual([...await host.read3dCacheFromHandle(root,'nested/demo.kicad_pro')],[1,2,3]);
  assert.equal(await host.write3dCacheToHandle(root,'nested/demo.kicad_pro',new Uint8Array([4,5])),true);
  assert.deepEqual([...host.projectCacheSection(new Uint8Array(writes[0]),'scene')],[4,5]);
  assert.equal(writes[1],'closed');
  assert.equal(requested,false);
  assert.equal(await host.write3dCacheToHandle({queryPermission:async()=> 'prompt'},'demo.kicad_pro',new Uint8Array([1])),false);
  await assert.rejects(host.write3dCacheToHandle(root,'../demo.kicad_pro',new Uint8Array([1])),/leaves project/);
});

test('read-only projects persist the binary 3D cache in IndexedDB without changing inventory',async()=>{
  const records={inventory:new Map([['current','inventory remains unchanged']]),folders:new Map()};
  globalThis.indexedDB={open:()=>{
    const request={};queueMicrotask(()=>{request.result={transaction:(name)=>{
      const tx={objectStore:()=>({
        get:key=>{const req={};queueMicrotask(()=>{req.result=records[name].get(key);req.onsuccess?.();});return req;},
        put:(value,key)=>{records[name].set(key,value);queueMicrotask(()=>tx.oncomplete?.());},
      })};return tx;
    }};request.onsuccess?.();});return request;
  }};
  records.folders.set('read-only',{queryPermission:async()=> 'denied'});
  const payload=btoa('prepared scene bytes');
  assert.match(await host.saveProject3dCache('read-only','project/demo.kicad_pro',payload),/browser/);
  assert.equal(await host.readProject3dCache('read-only','project/demo.kicad_pro'),payload);
  assert.equal(records.inventory.get('current'),'inventory remains unchanged');
  assert.equal(await host.readProject3dCache('read-only','another/demo.kicad_pro'),null);
  let stored=new Uint8Array([7,8]);
  const source=body=>{const blob=new Blob([body]);Object.defineProperty(blob,'lastModified',{value:123});return blob;};
  const root={queryPermission:async()=> 'granted',getFileHandle:async name=>{
    if(name==='.vspace.cache')return {getFile:async()=>new Blob([stored]),createWritable:async()=>{let pending;return {write:async bytes=>{pending=bytes;},close:async()=>{stored=pending;}};}};
    if(name.endsWith('.kicad_dru'))throw Object.assign(Error('missing'),{name:'NotFoundError'});
    return {getFile:async()=>source(name.endsWith('.kicad_pro')?'{}':'(kicad_pcb (version 20260206))')};
  }};
  records.folders.set('writable',root);
  assert.equal(await host.saveProjectPcbCache('writable','demo.kicad_pro',btoa('parsed PCB')),'Saved .vspace.cache');
  assert.deepEqual([...host.projectCacheSection(stored,'scene')],[7,8]);
  await host.saveProject3dCache('writable','demo.kicad_pro',btoa('prepared 3D'));
  assert.equal(new TextDecoder().decode(host.projectCacheSection(stored,'pcb')),'parsed PCB');
  const stamps=await host.pcbSourceStamps('writable','demo.kicad_pro');
  assert.equal(stamps.length,3);assert.equal(stamps[1].modified,'123000000');assert.equal(stamps[2].modified,'missing');

});

test('project cache sections preserve each other and validate metadata without decoding the board',()=>{
  const pcb=new Uint8Array([1,2,3]),scene=new Uint8Array([4,5]);const joined=host.packProjectCache(pcb,scene);
  assert.deepEqual([...host.projectCacheSection(joined,'pcb')],[1,2,3]);
  assert.deepEqual([...host.projectCacheSection(joined,'scene')],[4,5]);
  assert.deepEqual([...host.projectCacheSection(scene,'scene')],[4,5]);
  assert.equal(host.projectCacheSection(scene,'pcb').length,0);
  assert.throws(()=>host.projectCacheSection(joined.slice(0,-1),'pcb'),/lengths/);
  const stamps=[{path:'demo.kicad_pcb',size:999,modified:'123000000',header:[40,107]}];
  const meta=new TextEncoder().encode(JSON.stringify({version:1,sources:stamps}));
  const payload=new Uint8Array(12+meta.length+4);payload.set(new TextEncoder().encode('VINVPCB1'));new DataView(payload.buffer).setUint32(8,meta.length,true);payload.set(meta,12);
  const encoded=btoa(String.fromCharCode(...payload));
  assert.equal(host.parsedPcbCacheMatches(encoded,stamps,1),true);
  assert.equal(host.parsedPcbCacheMatches(encoded,stamps,2),false);
  for(const change of [{size:1000},{modified:'124000000'},{header:[40,108]}])assert.equal(host.parsedPcbCacheMatches(encoded,[{...stamps[0],...change}],1),false);
});

test('demo model manifest loads referenced assets only when requested', async () => {
  const {readBoardModels} = await import('./host.js');
  const reference = '${KIPRJMOD}/libs/models.3dshapes/2520 osc.STEP';
  const hidden = '${KIPRJMOD}/hidden.step';
  const board = {footprint_list: [{models: [{path: reference}, {path: hidden, hidden: true}]}]};
  const requested = [];
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async url => {
    requested.push(url.pathname);
    if (url.pathname.endsWith('/manifest.json')) return Response.json({
      project: 'CLN17-V2.5.kicad_pro',
      files: {[reference]: 'cln17/libs/models.3dshapes/2520 osc.STEP', [hidden]: 'cln17/hidden.step'},
    });
    return new Response('STEP');
  };
  try {
    assert.deepEqual(await readBoardModels(board, 'CLN17-V2.5.kicad_pro', null, null), {});
    assert.deepEqual(requested, []);
    assert.deepEqual(await readBoardModels(board, 'CLN17-V2.5.kicad_pro', null, null, null, true), {
      [reference]: ['cln17/libs/models.3dshapes/2520 osc.STEP', 'U1RFUA=='],
    });
    assert.equal(requested.length, 2);
    assert.match(requested[1], /2520%20osc\.STEP$/);
  } finally {
    globalThis.fetch = originalFetch;
  }
});

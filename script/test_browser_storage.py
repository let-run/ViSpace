#!/usr/bin/env python3
"""Isolated real-Chromium SQLite/OPFS integration check. Never uses a user's profile."""
import base64,json,pathlib,subprocess,tempfile,time,urllib.request,sqlite3,socket,websocket,sys,shutil
root=pathlib.Path(__file__).resolve().parents[1]
shutil.copyfile(root/'web/storage.integration.html',root/'dist/web/storage.integration.html')
with tempfile.TemporaryDirectory(prefix='inventory-browser-test-') as temp:
 sock=socket.socket();sock.bind(('127.0.0.1',0));port=sock.getsockname()[1];sock.close()
 server=subprocess.Popen(['python3',str(root/'script/serve_web.py'),'--directory',str(root/'dist/web'),'--port',str(port)],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
 chrome=subprocess.Popen(['/Applications/Google Chrome.app/Contents/MacOS/Google Chrome','--headless=new','--no-first-run','--no-default-browser-check','--enable-unsafe-swiftshader','--remote-debugging-port=0','--remote-allow-origins=*','--user-data-dir='+temp,'about:blank'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
 try:
  active=pathlib.Path(temp)/'DevToolsActivePort'
  for _ in range(200):
   if active.exists():break
   time.sleep(.1)
  debug=int(active.read_text().splitlines()[0]);url=f'http://127.0.0.1:{port}/storage.integration.html?storageTest=isolated-disposable-origin'
  target=json.load(urllib.request.urlopen(urllib.request.Request(f'http://127.0.0.1:{debug}/json/new?{url}',method='PUT')))
  ws=websocket.create_connection(target['webSocketDebuggerUrl'],timeout=120);counter=0
  def evaluate(expression):
   global counter
   counter+=1;ws.send(json.dumps({'id':counter,'method':'Runtime.evaluate','params':{'expression':expression,'awaitPromise':True,'returnByValue':True}}))
   while True:
    reply=json.loads(ws.recv())
    if reply.get('id')==counter:return reply.get('result',{}).get('result',{}).get('value')
  result=None
  for _ in range(180):
   result=evaluate('window.testResult');
   if result:break
   time.sleep(.5)
  if not result:raise RuntimeError('Browser test timeout: '+str(evaluate('document.body.innerText')))
  if not result.get('ok'):raise RuntimeError(result.get('error')+'\n'+result.get('text',''))
  print(result['text']);backup=evaluate('window.sqliteBackup');path=pathlib.Path(temp)/'backup.sqlite';path.write_bytes(base64.b64decode(backup));db=sqlite3.connect(path)
  assert db.execute('SELECT COUNT(*) FROM parts').fetchone()[0]==1
  assert db.execute('SELECT COUNT(*) FROM sync_events').fetchone()[0]>=2
  assert json.loads(db.execute('SELECT payload FROM parts').fetchone()[0])['datasheet'].startswith('asset:')
  print('PASS exported database opens in native SQLite with event history and asset references')
  if len(sys.argv)>1:
   destination=pathlib.Path(sys.argv[1]);destination.parent.mkdir(parents=True,exist_ok=True);destination.write_bytes(path.read_bytes())
  counter+=1;ws.send(json.dumps({'id':counter,'method':'Page.navigate','params':{'url':f'http://127.0.0.1:{port}/'}}))
  ready=False
  for _ in range(160):
   ready=evaluate("!document.querySelector('#loading') && !!globalThis.__inventorySqliteBridge")
   if ready:break
   time.sleep(.25)
  if not ready:raise RuntimeError('App UI failed to start: '+str(evaluate('document.body.innerText')))
  history = evaluate("""(async () => {
    const host = await import('./host.js');
    const {page: latest, author} = JSON.parse(await host.inventoryHistoryPage('null', 1));
    if (author !== (await host.inventorySync()).applicationId) throw Error('Wrong history author');
    const {page: older} = JSON.parse(await host.inventoryHistoryPage(JSON.stringify(latest.next), 1));
    return [latest, older];
  })()""")
  assert all(len(page['entries']) == 1 for page in history)
  latest, older = [page['entries'][0] for page in history]
  assert len(latest['hash']) == 40 and latest['entry']['record']['id'] != older['entry']['record']['id']
  print('PASS browser history bridge returns distinct pages with action hashes')
  print('PASS browser app starts with SQLite through the actual WASM host bridge')
  result=evaluate("(async()=>{const host=await import('./host.js');const wasm=await import('./pkg/vispace.js');const data=JSON.parse(wasm.export_inventory());data.parts[0].stock=9;wasm.import_inventory(JSON.stringify(data));for(let i=0;i<200;i++){await new Promise(r=>setTimeout(r,20));if(JSON.parse(host.inventorySnapshot()).parts[0].stock===9&&JSON.parse(wasm.export_inventory()).parts[0].stock===9){const bytes=await host.backupInventorySqlite();return new TextDecoder().decode(bytes.slice(0,15))==='SQLite format 3';}}return false;})()")
  if not result:raise RuntimeError('Actual app save/backup bridge failed: '+str(evaluate('document.body.innerText')))
  print('PASS actual WASM app saves, accepts the durable snapshot, and exports a SQLite backup')

  from test_git_bundle_browser import write_bundle
  bundle=pathlib.Path(temp)/'browser-test.bundle';bundle_tip=write_bundle(bundle,'{}')
  host_snippet=next((root/'dist/web/pkg/snippets').glob('*/web/host.js')).relative_to(root/'dist/web').as_posix()
  evaluate(f"""(() => {{
    window.bundleResult = null; window.bundleProjectId = crypto.randomUUID().toUpperCase();
    import('./{host_snippet}').then(host => host.pickGitBundles(window.bundleProjectId))
      .then(result => window.bundleResult = {{value:result}}, error => window.bundleResult = {{error:String(error)}});
    return true;
  }})()""")
  def protocol(method, params):
   global counter
   counter+=1;ws.send(json.dumps({'id':counter,'method':method,'params':params}))
   while True:
    reply=json.loads(ws.recv())
    if reply.get('id')==counter:
     if 'error' in reply:raise RuntimeError(reply['error'])
     return reply.get('result',{})
  for _ in range(40):
   if evaluate("""!!document.querySelector('input[accept=".bundle"]')"""):break
   time.sleep(.1)
  document=protocol('DOM.getDocument',{})['root']['nodeId']
  node=protocol('DOM.querySelector',{'nodeId':document,'selector':'input[accept=".bundle"]'})['nodeId']
  protocol('DOM.setFileInputFiles',{'nodeId':node,'files':[str(bundle)]})
  for _ in range(120):
   imported=evaluate('window.bundleResult')
   if imported:break
   time.sleep(.25)
  else:raise RuntimeError('Git bundle import worker did not complete')
  if imported.get('error'):raise RuntimeError(imported['error'])
  selection=json.loads(imported['value'])
  assert selection['choices']==['board.kicad_pro'],selection
  contents=evaluate(f"""(async()=>{{
    const host=await import('./{host_snippet}');
    return await host.readKiCadFolder({json.dumps(selection['folder'])},'schematics');
  }})()""")
  assert contents['board.kicad_pro']=='{}',contents
  print('PASS actual browser bundle worker imports through the wasm-bindgen host snippet')
  applied=evaluate(f"""(async()=>{{
    const wasm=await import('./pkg/vispace.js'),host=await import('./host.js');
    const data=JSON.parse(wasm.export_inventory());
    data.projects.push({{id:window.bundleProjectId,name:'Automatic Git project',description:'',git_tips:{{peer:{{reference:'refs/heads/main',commit:{json.dumps(bundle_tip)},includes:[]}}}}}});
    wasm.import_inventory(JSON.stringify(data));
    for(let i=0;i<160;i++){{
      await new Promise(resolve=>setTimeout(resolve,100));
      const link=JSON.parse(host.inventorySnapshot()).projects.find(p=>p.id===window.bundleProjectId)?.kicad;
      const visible=JSON.parse(wasm.export_inventory()).projects.find(p=>p.id===window.bundleProjectId)?.kicad;
      if(link?.browser_folder==={json.dumps(selection['folder'])}&&visible?.browser_folder===link.browser_folder)return link;
    }}
    return null;
  }})()""")
  assert applied and applied['path']=='board.kicad_pro',applied
  print('PASS cached received tip automatically attaches offline and updates the running WASM app')
  migrated=evaluate("""(async()=>{
    const wasm=await import('./pkg/vispace.js'),host=await import('./host.js');
    const data=JSON.parse(wasm.export_inventory());
    data.projects.find(p=>p.id===window.bundleProjectId).kicad.revision=crypto.randomUUID();
    wasm.import_inventory(JSON.stringify(data));
    await new Promise(resolve=>setTimeout(resolve,500));
    for(let i=0;i<160;i++){
      await new Promise(resolve=>setTimeout(resolve,100));
      const project=JSON.parse(wasm.export_inventory()).projects.find(p=>p.id===window.bundleProjectId);
      if(project.kicad.revision.startsWith('git-snapshot:'+window.bundleProjectId+':'))return true;
    }
    return false;
  })()""")
  assert migrated,'A manually opened Git snapshot did not resume automatic updates'
  print('PASS manually opened Git snapshots migrate back to automatic background updates')
  repaired=evaluate("""(async()=>{
    const wasm=await import('./pkg/vispace.js');const data=JSON.parse(wasm.export_inventory()),project=data.projects.find(p=>p.id===window.bundleProjectId),expected=project.kicad.browser_folder;
    const root=await navigator.storage.getDirectory(),snapshots=await root.getDirectoryHandle('git-kicad-snapshots'),name=crypto.randomUUID(),stale=await snapshots.getDirectoryHandle(name,{create:true});
    const db=await new Promise((resolve,reject)=>{const request=indexedDB.open('bom-inventory',1);request.onsuccess=()=>resolve(request.result);request.onerror=()=>reject(request.error);});
    await new Promise((resolve,reject)=>{const tx=db.transaction('folders','readwrite');tx.objectStore('folders').put(stale,name);tx.oncomplete=resolve;tx.onerror=()=>reject(tx.error);});
    project.kicad.browser_folder=name;wasm.import_inventory(JSON.stringify(data));
    for(let i=0;i<180;i++){await new Promise(resolve=>setTimeout(resolve,100));if(JSON.parse(wasm.export_inventory()).projects.find(p=>p.id===window.bundleProjectId).kicad.browser_folder===expected)return true;}return false;
  })()""")
  assert repaired,'A stale internal snapshot folder was trusted solely by its applied revision'
  print('PASS falsely current applied marker repairs the stale internal snapshot folder')





  local=evaluate("""(async()=>{
    const host=await import('./host.js'),root=await navigator.storage.getDirectory();
    const folder=await root.getDirectoryHandle('local-git-test',{create:true});
    async function write(path,text){const f=await folder.getFileHandle(path,{create:true}),w=await f.createWritable();await w.write(text);await w.close();}
    await write('board.kicad_pro','{}');await write('board.kicad_sch','(kicad_sch)');
    const ignored=await folder.getFileHandle('.vspace.cache',{create:true}),ignoredWriter=await ignored.createWritable();await ignoredWriter.truncate(33*1024*1024);await ignoredWriter.close();
    const large=await folder.getFileHandle('large-model.step',{create:true}),largeWriter=await large.createWritable();await largeWriter.truncate(160*1024*1024);await largeWriter.close();
    const db=await new Promise((resolve,reject)=>{const req=indexedDB.open('bom-inventory',1);req.onsuccess=()=>resolve(req.result);req.onerror=()=>reject(req.error);});
    await new Promise((resolve,reject)=>{const tx=db.transaction('folders','readwrite');tx.objectStore('folders').put(folder,'local-git-test');tx.oncomplete=resolve;tx.onerror=()=>reject(tx.error);});
    const id=crypto.randomUUID().toUpperCase();
    await host.runLocalGit('local-git-test',id,'init','','','');
    await write('.DS_Store','ignored');
    const history=await folder.getDirectoryHandle('.history',{create:true}),hf=await history.getFileHandle('board.kicad_sch',{create:true}),hw=await hf.createWritable();await hw.write('ignored history');await hw.close();
    const review=await host.runLocalGit('local-git-test',id,'status','','','');
    if(review.changes.some(p=>p==='.DS_Store'||p.startsWith('.history/')))throw Error('Git ignore rules were not respected');
    if(!review.change_details?.some(file=>file.path==='board.kicad_pro'&&file.kind==='Added'&&file.lines?.[0]===1&&file.lines?.[1]===0))throw Error('Added status or line counts missing from commit review');
    const first=await host.runLocalGit('local-git-test',id,'commit','Initial board','Browser Test','browser@example.invalid');
    if(first.io.maxRead>65536||first.io.maxAppend>65536||first.io.heapAfter-first.io.heapBefore>64*1024*1024)throw Error('Unbounded Git worker memory: '+JSON.stringify(first.io));
    console.log('Large Git streaming IO',JSON.stringify(first.io));
    const clean=await host.runLocalGit('local-git-test',id,'status','','','');
    if(clean.changes.length)throw Error('Fresh browser commit leaves changes: '+JSON.stringify(clean));
    await write('board.kicad_sch','(kicad_sch (version 20250114))');
    const modified=await host.runLocalGit('local-git-test',id,'status','','','');
    if(!modified.change_details?.some(file=>file.path==='board.kicad_sch'&&file.kind==='Modified'&&file.lines?.[0]===1&&file.lines?.[1]===1))throw Error('Modified status or line counts missing from commit review');
    const second=await host.runLocalGit('local-git-test',id,'commit','Update board','Browser Test','browser@example.invalid');
    if(second.head!==first.tip||second.tip===first.tip)throw Error('Second commit did not extend first');
    await host.runLocalGit('local-git-test',id,'import','','','');
    const git=await folder.getDirectoryHandle('.git'),refs=await(await(await git.getDirectoryHandle('refs')).getDirectoryHandle('vinventory')).getDirectoryHandle('bundles');
    if((await(await refs.getFileHandle(second.tip)).getFile()).size<40)throw Error('Received Git history was not imported');
    const head=await(await(await(await(await git.getDirectoryHandle('refs')).getDirectoryHandle('heads')).getFileHandle('main')).getFile()).text();
    if(head.trim()!==second.tip)throw Error('Import changed the working branch');
    const sentinel=await git.getFileHandle('index.lock',{create:true}),sw=await sentinel.createWritable();await sw.write('another writer');await sw.close();
    let refused=false;try{await host.runLocalGit('local-git-test',id,'commit','Must fail','Browser Test','browser@example.invalid');}catch(error){refused=String(error).includes('locked');}
    if(!refused||await(await sentinel.getFile()).text()!=='another writer')throw Error('Active Git writer lock was not preserved');
    await git.removeEntry('index.lock');
    const files={};async function scan(dir,prefix){for await(const[name,entry]of dir.entries()){if(entry.kind==='directory')await scan(entry,prefix+name+'/');else{if(['.vspace.cache','large-model.step'].includes(prefix+name))continue;const bytes=new Uint8Array(await(await entry.getFile()).arrayBuffer());let binary='';for(let i=0;i<bytes.length;i+=32768)binary+=String.fromCharCode(...bytes.subarray(i,i+32768));files[prefix+name]=btoa(binary);}}}
    await scan(folder,'');return {files,first:first.tip,second:second.tip,io:first.io};
  })().catch(error=>({error:String(error),stack:error.stack}))""")
  assert local and 'files' in local,local
  checkout=pathlib.Path(temp)/'browser-written-repository';checkout.mkdir()
  for relative,encoded in local['files'].items():
   dest=checkout/relative;dest.parent.mkdir(parents=True,exist_ok=True);dest.write_bytes(base64.b64decode(encoded))
  verified=json.loads(subprocess.check_output([str(root/'target/debug/examples/browser_git_verify'),str(checkout)],text=True))
  assert verified['tip']==local['second'] and verified['parents']==[local['first']],verified
  print('PASS 160 MiB tracked file uses bounded chunks and WASM memory',local['io'])
  print('PASS browser init and two commits write a standard repository readable by desktop Grit')

  identity=evaluate("(async()=> (await (await import('./host.js')).inventorySync()).applicationId)()")
  evaluate("""(() => {
    window.confirm = () => true;
    import('./host.js').then(host => host.resetBrowserInventory()).catch(error => window.resetError = String(error));
    return true;
  })()""")
  for _ in range(160):
   reset=evaluate("""(async () => {
     if (window.resetError) return {error:window.resetError};
     if (!globalThis.__inventorySqliteBridge) return null;
     const host = await import('./host.js');
     const state = await host.inventorySync();
     const data = JSON.parse(host.inventorySnapshot());
     const root = await navigator.storage.getDirectory();
     const workspace = await root.getDirectoryHandle('bom-inventory-workspace');
     let hasAttachments = false;
     for await (const name of workspace.keys()) if (name === 'Attachments') hasAttachments = true;
     const history = JSON.parse(await host.inventoryHistoryPage('null', 20)).page.entries;
     return {identity:state.applicationId, parts:data.parts.length, hasAttachments, records:history.length};
   })()""")
   if reset and 'error' in reset:raise RuntimeError(reset['error'])
   if reset and reset['identity'] != identity:break
   time.sleep(.25)
  else:raise RuntimeError(f'Browser inventory did not reload with a fresh identity: {reset!r}; {evaluate("document.body.innerText")!r}')
  assert reset['parts']==0 and not reset['hasAttachments'] and reset['records']<=1,reset
  print('PASS reset clears inventory, attachments, and prior action history')

 finally:
  chrome.terminate();server.terminate();chrome.wait(timeout=10);server.wait(timeout=10)

#!/usr/bin/env python3
"""Browser-authored Git bundle upload in an isolated Chrome profile and temporary databases.
Build first: cargo build --manifest-path Cargo.toml --example browser_sync_peer
            ./script/web.sh --build-only
"""
import base64, hashlib, json, os, pathlib, socket, sys, subprocess, tempfile, time, urllib.request, uuid, websocket
root=pathlib.Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix='inventory-sync-test-') as temp:
 temp=pathlib.Path(temp);processes=[]
 try:
  def peer(name):
   folder=temp/name;folder.mkdir();p=subprocess.Popen([str(root/'target/debug/examples/browser_sync_peer'),str(folder)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True);processes.append(p);return p,json.loads(p.stdout.readline())['url'],folder
  a,url_a,folder_a=peer('a')
  def command(p,op,**args):
   p.stdin.write(json.dumps(dict(op=op,**args))+'\n');p.stdin.flush();r=json.loads(p.stdout.readline());
   if 'error' in r:raise RuntimeError(r['error'])
   return r['value']
  png=base64.b64decode('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=');pdf=b'%PDF-1.7 native fixture'
  image=command(a,'asset',path='native/photo.png',bytes=list(png));sheet=command(a,'asset',path='native/sheet.pdf',bytes=list(pdf))
  data=command(a,'get')['inventory'];data['parts']=[dict(id=str(uuid.uuid4()).upper(),barcode=str(uuid.uuid4()).upper(),number='SYNC-1',stock=5,testNumber=1.0,description='Common description',image=dict(originalPath=image,previewPath=image,thumbnailPath=image),datasheet=sheet,notes=dict(text='line one\nline two\n',markdownSource='line one\nline two\n'))];command(a,'set',inventory=data)
  if os.environ.get('TEST_PARTITION_SYNC'):
   command(a,'enable_partition');time.sleep(3)
  sock=socket.socket();sock.bind(('127.0.0.1',0));port=sock.getsockname()[1];sock.close()
  server=subprocess.Popen(['python3',str(root/'script/serve_web.py'),'--directory',str(root/'dist/web'),'--port',str(port)],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);processes.append(server)
  pub=subprocess.check_output(['openssl','x509','-in',str(root/'.local-tls/server.pem'),'-pubkey','-noout']);der=subprocess.check_output(['openssl','pkey','-pubin','-outform','DER'],input=pub);spki=base64.b64encode(hashlib.sha256(der).digest()).decode()
  profile=temp/'chrome';chrome=subprocess.Popen(['/Applications/Google Chrome.app/Contents/MacOS/Google Chrome','--headless=new','--ignore-certificate-errors-spki-list='+spki,'--window-size=1400,900','--no-first-run','--no-default-browser-check','--enable-unsafe-swiftshader','--remote-debugging-port=0','--remote-allow-origins=*','--user-data-dir='+str(profile),'about:blank'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);processes.append(chrome)
  active=profile/'DevToolsActivePort'
  for _ in range(200):
   if active.exists():break
   time.sleep(.1)
  debug=int(active.read_text().splitlines()[0]);target=json.load(urllib.request.urlopen(urllib.request.Request(f'http://127.0.0.1:{debug}/json/new?https://localhost:{port}/',method='PUT')));ws=websocket.create_connection(target['webSocketDebuggerUrl'],timeout=90);counter=0
  def evaluate(expression):
   global counter
   counter+=1;ws.send(json.dumps(dict(id=counter,method='Runtime.evaluate',params=dict(expression=expression,awaitPromise=True,returnByValue=True))))
   while True:
    reply=json.loads(ws.recv())
    if reply.get('id')==counter:
     result=reply.get('result',{})
     if result.get('exceptionDetails'):raise RuntimeError(str(result['exceptionDetails']))
     return result.get('result',{}).get('value')
  def cdp(method,params):
   global counter
   counter+=1;ws.send(json.dumps(dict(id=counter,method=method,params=params)))
   while True:
    reply=json.loads(ws.recv())
    if reply.get('id')==counter:return reply.get('result',{})
  def until(test,label):
   end=time.time()+30
   while time.time()<end:
    if test():print('PASS '+label,flush=True);return
    time.sleep(.1)
   raise RuntimeError(label+' timed out; '+str(evaluate('document.body.innerText')))
  until(lambda:evaluate("!document.querySelector('#loading') && !!globalThis.__inventorySqliteBridge"),'WASM app starts')
  evaluate("(async()=>{window.host=await import('./host.js');window.wasm=await import('./pkg/vispace.js');return true;})()")
  def sync(op='state',**args):
   result=evaluate(f'host.inventorySync({json.dumps(op)},{json.dumps(args)})')
   if op=='connect':
    native=a if args['url']==url_a else b
    until(lambda:bool(command(native,'security')['pending']),'pending signed pairing appears')
    info=command(native,'security');browser_id=result['applicationId'];public=next(key for node,key in info['pending'] if node==browser_id)
    until(lambda:any(node['trusted'] for node in sync()['security']['pending'] if node['public_key']==info['public_key']),'browser initiator approves the verified desktop')
    command(native,'authorize',public_key=public)
   return result
  def edit(script):
   # The synchronous Rust editor deliberately pauses while an async projection is adopted.
   return evaluate("(async()=>{for(let i=0;i<400;i++){try{const d=JSON.parse(wasm.export_inventory());"+script+";wasm.import_inventory(JSON.stringify(d));return true;}catch(e){if(!String(e).includes('Sync is applying'))throw e;await new Promise(r=>setTimeout(r,25));}}throw Error('Sync never released the editor');})()")
  sync('connect',url=url_a)
  until(lambda:evaluate('JSON.parse(wasm.export_inventory()).parts.length===1'),'native history reaches browser')
  x,y=evaluate("(()=>{const r=document.querySelector('#app').getBoundingClientRect();return [r.right-40,r.bottom-16];})()")
  for event in ['mouseMoved','mousePressed','mouseReleased']:
   cdp('Input.dispatchMouseEvent',dict(type=event,x=x,y=y,button='none' if event=='mouseMoved' else 'left',clickCount=1))
  until(lambda:evaluate("document.querySelector('.inventory-sync-dialog')?.open && document.querySelector('#sync-title').textContent.startsWith('Sync · ') && document.querySelector('#sync-nearby').textContent.includes('Approved')"),'bottom-bar Sync opens Discover with the approved desktop')
  assert evaluate("(()=>{document.querySelector('#sync-tab-connect').click();return !document.querySelector('#sync-connect').hidden && document.querySelector('#sync-discover').hidden && !!document.querySelector('#sync-url');})()")
  evaluate("document.querySelector('#sync-close').click();true")
  authored=evaluate("""(async()=>{
    const root=await navigator.storage.getDirectory(),folder=await root.getDirectoryHandle('network-local-git',{create:true});
    const file=await folder.getFileHandle('network.kicad_pro',{create:true}),writer=await file.createWritable();await writer.write('{}');await writer.close();
    for(const name of ['~network.kicad_pro.lck','~network.kicad_sch.lck']){const lock=await folder.getFileHandle(name,{create:true}),w=await lock.createWritable();await w.write('temporary KiCad process lock');await w.close();}
    const large=await folder.getFileHandle('component.step',{create:true}),largeWriter=await large.createWritable();for(let i=0;i<48;i++)await largeWriter.write(crypto.getRandomValues(new Uint8Array(65536)));await largeWriter.close();
    const db=await new Promise((resolve,reject)=>{const r=indexedDB.open('bom-inventory',1);r.onsuccess=()=>resolve(r.result);r.onerror=()=>reject(r.error);});
    await new Promise((resolve,reject)=>{const tx=db.transaction('folders','readwrite');tx.objectStore('folders').put(folder,'network-local-git');tx.oncomplete=resolve;tx.onerror=()=>reject(tx.error);});
    let id=crypto.randomUUID().toUpperCase();
    await host.runLocalGit('network-local-git',id,'init','','','');
    const initial=await host.runLocalGit('network-local-git',id,'commit','Browser-authored project','Browser Test','browser@example.invalid');
    id=crypto.randomUUID().toUpperCase();
    const commit=await host.runLocalGit('network-local-git',id,'share','','','');
    if(commit.tip!==initial.tip||commit.head!==initial.tip)throw Error('Attaching an existing repository changed its HEAD');
    window.browserAuthoredGit={id,commit};return {id,commit};
  })().catch(error=>({error:String(error)}))""")
  assert authored and 'commit' in authored,authored
  edit("d.projects.push({id:window.browserAuthoredGit.id,name:'Browser authored',description:'Local browser Git',kicad:{path:'network.kicad_pro',revision:'local-browser',files:{},browser_folder:'network-local-git'},git_tips:{[window.browserAuthoredGit.commit.publisher]:{reference:window.browserAuthoredGit.commit.reference,commit:window.browserAuthoredGit.commit.tip,includes:[]}}})")
  cache_a=folder_a/'inventory.git-bundles'/authored['id'].lower()/('full-'+authored['commit']['tip']+'.bundle')
  until(lambda:cache_a.exists(),'attached existing browser repository shares its HEAD bundle with desktop')
  def desktop_snapshot(text):
   project=next((p for p in command(a,'get')['inventory']['projects'] if p['id']==authored['id']),{})
   link=project.get('kicad');return bool(link and pathlib.Path(link['path']).is_file() and pathlib.Path(link['path']).read_text()==text)
  until(lambda:desktop_snapshot('{}'),'desktop automatically attaches received KiCad files without a local checkout')
  first_checkout=next(p for p in command(a,'get')['inventory']['projects'] if p['id']==authored['id'])['kicad']['path']



  second=evaluate("""(async()=>{
    const root=await navigator.storage.getDirectory(),folder=await root.getDirectoryHandle('network-local-git');
    const f=await folder.getFileHandle('network.kicad_pro'),writer=await f.createWritable();await writer.write('{"meta":{"filename":"network.kicad_pro"}}');await writer.close();
    const commit=await host.runLocalGit('network-local-git',window.browserAuthoredGit.id,'commit','Second browser commit','Browser Test','browser@example.invalid');
    // Re-publication can produce different pack ordering for the same commit.
    // Its immutable payload must not replace bytes referenced by an older job.
    const db=await new Promise((resolve,reject)=>{const r=indexedDB.open('bom-inventory',1);r.onsuccess=()=>resolve(r.result);r.onerror=()=>reject(r.error);});
    const read=key=>new Promise((resolve,reject)=>{const r=db.transaction('inventory').objectStore('inventory').get(key);r.onsuccess=()=>resolve(r.result);r.onerror=()=>reject(r.error);});
    const key='git-outbox:'+window.browserAuthoredGit.id,before=await read(key),outbox=await root.getDirectoryHandle('git-outbox');
    const original=await(await outbox.getFileHandle(before.file)).getFile(),originalBytes=await original.arrayBuffer();
    await host.runLocalGit('network-local-git',window.browserAuthoredGit.id,'share','','','');
    const after=await read(key),same=await(await outbox.getFileHandle(before.file)).getFile();
    if(after.file!==before.file||after.deltaFile!==before.deltaFile||new Uint8Array(await original.arrayBuffer()).some((b,i)=>b!==new Uint8Array(originalBytes)[i])||new Uint8Array(await same.arrayBuffer()).some((b,i)=>b!==new Uint8Array(originalBytes)[i]))throw Error('Re-publication replaced an active bundle payload');
    // Simulate the legacy checksum bug with valid equivalent bundle bytes.
    for(const [field,checksum]of [['file','sha256'],['deltaFile','deltaSha256']]){
      if(!after[field])continue;
      const bytes=new Uint8Array(await(await(await outbox.getFileHandle(after[field])).getFile()).arrayBuffer()),needle=new TextEncoder().encode('refs/heads/main');
      let pos=-1;for(let i=0;i<Math.min(bytes.length,4096)-needle.length;i++)if(needle.every((v,j)=>bytes[i+j]===v)){pos=i;break;}
      if(pos<0)throw Error('Missing fixture bundle reference');bytes.set(new TextEncoder().encode('refs/heads/test'),pos);
      const legacy=await outbox.getFileHandle('legacy-'+field+'.bundle',{create:true}),writer=await legacy.createWritable();await writer.write(bytes);await writer.close();after[field]=legacy.name;
    }
    await new Promise((resolve,reject)=>{const tx=db.transaction('inventory','readwrite');tx.objectStore('inventory').put(after,key);tx.oncomplete=resolve;tx.onerror=()=>reject(tx.error);});
    window.browserSecondCommit=commit;return commit;
  })().catch(error=>({error:String(error)}))""")
  assert second and second.get('head')==authored['commit']['tip'],second
  second_begin=time.monotonic()
  edit("d.projects.find(p=>p.id===window.browserAuthoredGit.id).git_tips[window.browserSecondCommit.publisher]={reference:window.browserSecondCommit.reference,commit:window.browserSecondCommit.tip,includes:[window.browserAuthoredGit.commit.tip]}")
  delta=cache_a.parent/(second['head']+'-'+second['tip']+'.bundle')
  full=cache_a.parent/('full-'+second['tip']+'.bundle')
  until(lambda:delta.exists(),'second browser commit sends an incremental bundle without eagerly rebuilding full history')
  until(lambda:desktop_snapshot('{"meta":{"filename":"network.kicad_pro"}}'),'desktop automatically advances the attached snapshot after the next commit')
  print('PASS same-tip publication preserves active payloads and legacy stale checksums recover before upload',flush=True)
  desktop_project=next(p for p in command(a,'get')['inventory']['projects'] if p['id']==authored['id'])
  checkout=pathlib.Path(desktop_project['kicad']['path']).parent
  assert desktop_project['kicad']['path']==first_checkout,(first_checkout,desktop_project['kicad']['path'])
  assert checkout.name=='repository',checkout
  assert (checkout/'component.step').stat().st_size==3*1024*1024
  verified=json.loads(subprocess.check_output([str(root/'target/debug/examples/browser_git_verify'),str(checkout)],text=True))
  assert verified['tip']==second['tip'] and verified['parents']==[second['head']],verified
  print('PASS desktop checkout contains all tracked files, a Git index, and complete parent history',flush=True)

  assert ('-'+second['head']).encode() in delta.read_bytes().split(b'\n\n',1)[0]
  assert delta.stat().st_size<64*1024,delta.stat().st_size
  assert not full.exists(),'Full-history bundle was rebuilt eagerly'
  snippet=next((root/'dist/web/pkg/snippets').glob('*/web/host.js')).relative_to(root/'dist/web').as_posix()
  evaluate(f"(async()=>{{window.bundleHost=await import('./{snippet}');return true;}})()")
  downloaded=evaluate("""(async()=>{
    const id=window.browserAuthoredGit.id,tip=window.browserSecondCommit.tip;
    const db=await new Promise((resolve,reject)=>{const r=indexedDB.open('bom-inventory',1);r.onsuccess=()=>resolve(r.result);r.onerror=()=>reject(r.error);});
    await new Promise((resolve,reject)=>{const tx=db.transaction('inventory','readwrite');tx.objectStore('inventory').delete('git-snapshot:'+id+':'+tip);tx.oncomplete=resolve;tx.onerror=()=>reject(tx.error);});
    localStorage.removeItem('vinventory.git-base:'+id);
    return JSON.parse(await bundleHost.fetchGitBundle(id,tip));
  })().catch(error=>({error:String(error)}))""")
  assert downloaded and downloaded.get('folder'),downloaded

  print('PASS multi-megabyte upload and download cross acknowledgement windows with streamed validation',flush=True)

  print(f'PASS subsequent browser commit transfers only the missing range ({time.monotonic()-second_begin:.2f}s after announcement)',flush=True)
  receiver=evaluate("""(async()=>JSON.parse(await bundleHost.fetchGitBundle(window.browserAuthoredGit.id,window.browserSecondCommit.tip)))()""")
  evaluate('window.receiverSelection='+json.dumps(receiver))
  # Keep the original browser checkout attached: remote desktop commits must advance it too.
  third=command(a,'commit_project',project=authored['id'],text='{"meta":{"filename":"desktop-update"}}')['tip']
  # Recover a legacy projection that claimed application before checkout finished.
  edit("{const p=d.projects.find(p=>p.id===window.browserAuthoredGit.id);p.kicad.revision='git-snapshot:'+p.id+':"+third+"';}")
  until(lambda:evaluate('JSON.parse(wasm.export_inventory()).projects.find(p=>p.id===window.browserAuthoredGit.id).kicad.revision.endsWith('+json.dumps(third)+')'),'desktop commits from a received checkout are announced and auto-applied by browser')
  until(lambda:evaluate("(async()=>{const root=await navigator.storage.getDirectory();const folder=await root.getDirectoryHandle('network-local-git');return (await(await(await folder.getFileHandle('network.kicad_pro')).getFile()).text()).includes('desktop-update');})()"),'legacy applied marker cannot suppress the physical checkout update')
  diagnostics=evaluate("(async()=>await bundleHost.runLocalGit('',window.browserAuthoredGit.id,'diagnostics','','',''))()")
  assert diagnostics['head']==third and any(entry['hash']==third and entry['message'] for entry in diagnostics['history']),diagnostics
  assert diagnostics['bundles'] and any(event['phase']=='Applied' for event in diagnostics['events']),diagnostics
  page=evaluate("(async()=>await bundleHost.runLocalGit('',window.browserAuthoredGit.id,'diagnostics','1','',''))()")
  assert len(page['history'])==1 and page['history_more'] and page['history'][0]==diagnostics['history'][0],page
  assert diagnostics['history_more']==False,diagnostics
  print('PASS history pagination returns the requested page size and detects the end of history',flush=True)
  readonly=evaluate("bundleHost.runLocalGit(window.receiverSelection.folder,window.browserAuthoredGit.id,'status','','','')")
  assert readonly.get('read_only') and not readonly['initialized'],readonly
  print('PASS Git diagnostics report actual checkout, commit history, received ranges and application activity; internal snapshots remain read-only',flush=True)
  relocated=evaluate("""(async()=>{
    const root=await navigator.storage.getDirectory();window.testDestination=await root.getDirectoryHandle('relocated-receiver',{create:true});
    window.showDirectoryPicker=async()=>window.testDestination;
    const p=JSON.parse(wasm.export_inventory()).projects.find(p=>p.id===window.browserAuthoredGit.id);
    const selection=JSON.parse(await bundleHost.pickManagedKiCadFolder(p.id,p.kicad.revision.split(':')[2]));
    window.relocatedSelection=selection;return selection;
  })().catch(error=>({error:String(error)}))""")
  assert relocated and relocated.get('managed_revision'),relocated
  lockfiles=evaluate("""(async()=>{
    const directory=window.testDestination,git=await directory.getDirectoryHandle('.git');
    const index=new TextDecoder().decode(await(await(await git.getFileHandle('index')).getFile()).arrayBuffer());
    let omitted=true;for(const name of ['~network.kicad_pro.lck','~network.kicad_sch.lck']){try{await directory.getFileHandle(name);omitted=false;}catch(error){if(error.name!=='NotFoundError')throw error;}}
    return {omitted,retained:index.includes('~network.kicad_pro.lck')&&index.includes('~network.kicad_sch.lck')};
  })()""")
  assert lockfiles=={'omitted':True,'retained':True},lockfiles
  print('PASS KiCad temporary lockfiles stay in Git history/index without browser filesystem writes',flush=True)
  edit("{const p=d.projects.find(p=>p.id===window.browserAuthoredGit.id);p.kicad={path:window.relocatedSelection.choices[0],revision:window.relocatedSelection.managed_revision,files:{},browser_folder:window.relocatedSelection.folder};}")
  matches=evaluate("""(async()=>{
    const p=JSON.parse(wasm.export_inventory()).projects.find(p=>p.id===window.browserAuthoredGit.id),tip=p.kicad.revision.split(':')[2];
    const accepted=JSON.parse(await bundleHost.pickManagedKiCadFolder(p.id,tip));
    const root=await navigator.storage.getDirectory(),invalid=await root.getDirectoryHandle('invalid-receiver',{create:true});
    const f=await invalid.getFileHandle('keep.txt',{create:true}),w=await f.createWritable();await w.write('keep');await w.close();
    window.showDirectoryPicker=async()=>invalid;
    let refused=false;try{await bundleHost.pickManagedKiCadFolder(p.id,tip);}catch{refused=true;}
    const wrong=await root.getDirectoryHandle('wrong-head-receiver',{create:true}),wf=await wrong.getFileHandle('wrong.kicad_pro',{create:true}),ww=await wf.createWritable();await ww.write('{}');await ww.close();
    const wrongId=crypto.randomUUID().toUpperCase(),folder=crypto.randomUUID();
    const db=await new Promise((resolve,reject)=>{const r=indexedDB.open('bom-inventory',1);r.onsuccess=()=>resolve(r.result);r.onerror=()=>reject(r.error);});
    await new Promise((resolve,reject)=>{const tx=db.transaction('folders','readwrite');tx.objectStore('folders').put(wrong,folder);tx.oncomplete=resolve;tx.onerror=()=>reject(tx.error);});
    await host.runLocalGit(folder,wrongId,'init','','','');await host.runLocalGit(folder,wrongId,'commit','Unrelated repository','Test','test@example.invalid');
    window.showDirectoryPicker=async()=>wrong;
    let wrongRefused=false;try{await bundleHost.pickManagedKiCadFolder(p.id,tip);}catch{wrongRefused=true;}
    return {accepted:!!accepted.folder,refused,wrongRefused,preserved:await(await f.getFile()).text()};
  })().catch(error=>({error:String(error)}))""")
  assert matches=={'accepted':True,'refused':True,'wrongRefused':True,'preserved':'keep'},matches
  resumed=evaluate("""(async()=>{
    const p=JSON.parse(wasm.export_inventory()).projects.find(p=>p.id===window.browserAuthoredGit.id),tip=p.kicad.revision.split(':')[2],root=await navigator.storage.getDirectory();
    const destination=await root.getDirectoryHandle('interrupted-receiver',{create:true});window.showDirectoryPicker=async()=>destination;
    const prototype=FileSystemDirectoryHandle.prototype,original=prototype.getFileHandle;let once=true,context=false;
    prototype.getFileHandle=async function(name,options){if(once&&this.name==='.git'&&name==='index'&&options?.create){once=false;throw new TypeError('Name is not allowed.');}return original.call(this,name,options);};
    try{await bundleHost.pickManagedKiCadFolder(p.id,tip);}catch(error){context=String(error).includes('.git/index');}finally{prototype.getFileHandle=original;}
    const repaired=JSON.parse(await bundleHost.pickManagedKiCadFolder(p.id,tip));
    const review=await host.runLocalGit(repaired.folder,p.id,'status','','','');
    const git=await destination.getDirectoryHandle('.git');await git.removeEntry('index');const heads=await(await git.getDirectoryHandle('refs')).getDirectoryHandle('heads');await heads.removeEntry('main');
    const file=await destination.getFileHandle('network.kicad_pro'),writer=await file.createWritable();await writer.write('local edits');await writer.close();
    let refused=false;try{await bundleHost.pickManagedKiCadFolder(p.id,tip);}catch{refused=true;}
    return {context,head:review.head,expected:tip,clean:review.changes.length===0,refused,contents:await(await file.getFile()).text()};
  })()""")
  assert resumed['context'] and resumed['head']==resumed['expected'] and resumed['clean'] and resumed['refused'] and resumed['contents']=='local edits',resumed
  print('PASS interrupted clone resumes a complete Git repository and preserves changed files on rejection',flush=True)

  evaluate("""(async()=>{
    const p=JSON.parse(wasm.export_inventory()).projects.find(p=>p.id===window.browserAuthoredGit.id),root=await navigator.storage.getDirectory();
    const directory=await root.getDirectoryHandle('older-interrupted',{create:true});window.showDirectoryPicker=async()=>directory;
    await bundleHost.pickManagedKiCadFolder(p.id,p.kicad.revision.split(':')[2]);
    const git=await directory.getDirectoryHandle('.git');await git.removeEntry('index');await(await git.getDirectoryHandle('refs')).getDirectoryHandle('heads').then(heads=>heads.removeEntry('main'));
  })()""")
  # Older installations recorded managed folders without an applied commit suffix.
  edit("{const p=d.projects.find(p=>p.id===window.browserAuthoredGit.id);p.kicad.revision='git-snapshot:'+p.id;}")
  fourth=command(a,'commit_project',project=authored['id'],text='{"meta":{"filename":"relocated-update"}}')['tip']
  until(lambda:evaluate('JSON.parse(wasm.export_inventory()).projects.find(p=>p.id===window.browserAuthoredGit.id).kicad.revision.endsWith('+json.dumps(fourth)+')'),'browser keeps updating the relocated physical repository')
  older=evaluate("""(async()=>{
    const p=JSON.parse(wasm.export_inventory()).projects.find(p=>p.id===window.browserAuthoredGit.id),root=await navigator.storage.getDirectory();
    window.showDirectoryPicker=async()=>root.getDirectoryHandle('older-interrupted');
    const selection=JSON.parse(await bundleHost.pickManagedKiCadFolder(p.id,p.kicad.revision.split(':')[2]));
    const status=await host.runLocalGit(selection.folder,p.id,'status','','','');return {head:status.head,clean:status.changes.length===0};
  })().catch(error=>({error:String(error)}))""")
  assert older=={'head':fourth,'clean':True},older
  print('PASS interrupted older checkout recovers then fast-forwards to latest received commit',flush=True)
  contents=evaluate("""(async()=>{
    const p=JSON.parse(wasm.export_inventory()).projects.find(p=>p.id===window.browserAuthoredGit.id);
    return {folder:p.kicad.browser_folder,text:(await host.readKiCadFolder(p.kicad.browser_folder,'schematics'))[p.kicad.path]};
  })()""")
  assert contents['folder']==relocated['folder'] and 'relocated-update' in contents['text'],contents
  print('PASS browser relocation copies full repository, accepts matching HEAD, rejects non-Git content, and keeps updates active',flush=True)
  garbage=evaluate("""(async()=>{
    const p=JSON.parse(wasm.export_inventory()).projects.find(p=>p.id===window.browserAuthoredGit.id),root=await navigator.storage.getDirectory(),outbox=await root.getDirectoryHandle('git-outbox');
    const orphan=await outbox.getFileHandle(p.id+'-obsolete.bundle',{create:true}),w=await orphan.createWritable();await w.write('obsolete cache');await w.close();
    const snapshots=await root.getDirectoryHandle('git-kicad-snapshots'),abandoned=await snapshots.getDirectoryHandle(crypto.randomUUID(),{create:true});
    const sf=await abandoned.getFileHandle('abandoned.kicad_pro',{create:true}),sw=await sf.createWritable();await sw.write('{}');await sw.close();
    bundleHost.setKeepGitBundles(true);await bundleHost.cleanupGitStorage({force:true,minAge:0});
    await outbox.getFileHandle(orphan.name);
    bundleHost.setKeepGitBundles(false);const cleaned=await bundleHost.cleanupGitStorage({force:true,minAge:0});
    let removed=false;try{await outbox.getFileHandle(orphan.name);}catch(error){removed=error.name==='NotFoundError';}
    let snapshotRemoved=false;try{await snapshots.getDirectoryHandle(abandoned.name);}catch(error){snapshotRemoved=error.name==='NotFoundError';}
    const cache=await(await root.getDirectoryHandle('git-bundles')).getDirectoryHandle(p.id);
    for await(const entry of cache.values())await cache.removeEntry(entry.name);
    const regenerated=await bundleHost.runLocalGit(p.kicad.browser_folder,p.id,'cache','','','');
    let files=0;for await(const entry of cache.values())if(entry.name.endsWith('.bundle'))files++;
    const status=await bundleHost.runLocalGit(p.kicad.browser_folder,p.id,'status','','','');
    return {removed,snapshotRemoved,head:regenerated.head,expected:status.head,clean:status.changes.length===0,files,cleaned};
  })().catch(error=>({error:String(error)}))""")
  assert garbage['removed'] and garbage['snapshotRemoved'] and garbage['head']==garbage['expected'] and garbage['clean'] and garbage['files']==1,garbage
  assert garbage['cleaned']['bundles']>0,garbage
  print('PASS cache cleanup preserves opted-in bundles and current jobs, prunes obsolete files, and regenerates a full bundle from repository objects',flush=True)

  # Both nodes commit from the same ancestor. Preserve each branch, import the
  # competing histories in refs, then propagate a user's two-parent resolution.
  browser_branch=evaluate("""(async()=>{
    const p=JSON.parse(wasm.export_inventory()).projects.find(p=>p.id===window.browserAuthoredGit.id),root=await navigator.storage.getDirectory(),folder=await root.getDirectoryHandle('relocated-receiver');
    const writer=await(await folder.getFileHandle('network.kicad_pro')).createWritable();await writer.write('{"meta":{"filename":"browser-concurrent"}}');await writer.close();
    return await host.runLocalGit(p.kicad.browser_folder,p.id,'commit','Browser concurrent edit','Browser Test','browser@example.invalid');
  })()""")
  competing=command(a,'commit_project',project=authored['id'],base=fourth,text='{"meta":{"filename":"desktop-concurrent"}}')['tip']
  until(lambda:len(next(p for p in command(a,'get')['inventory']['projects'] if p['id']==authored['id'])['git_tips'])>=2,'concurrent branch announcements reach desktop')
  def resolve_when_imported():
   try:return command(a,'merge_project',project=authored['id'],other=browser_branch['tip'])['tip']
   except RuntimeError:return None
  resolution=None
  end=time.time()+35
  while time.time()<end and not resolution:
   resolution=resolve_when_imported()
   if not resolution:time.sleep(.2)
  assert resolution,'Competing browser history was not imported into desktop refs'
  until(lambda:evaluate('JSON.parse(wasm.export_inventory()).projects.find(p=>p.id===window.browserAuthoredGit.id).kicad.revision.endsWith('+json.dumps(resolution)+')'),'manual resolution containing both histories propagates back to browser')
  print('PASS competing histories retained in refs and a manual merge resolution converges',flush=True)
  # Recreating the worker must reconnect without losing the gateway or identity.
  cdp('Page.reload',{});time.sleep(.5)
  until(lambda:evaluate("!document.querySelector('#loading') && !!globalThis.__inventorySqliteBridge"),'browser reload finishes')
  evaluate("(async()=>{window.host=await import('./host.js');window.wasm=await import('./pkg/vispace.js');return true;})()")
  until(lambda:evaluate("host.inventorySync().then(state=>state.peers.some(peer=>peer.status==='Live'&&peer.gitBundles))"),'browser reconnects with bundle capability after reload')
  browser_id=sync()['applicationId'];info=command(a,'security');browser_key=next(key for node,key in info['pending'] if node==browser_id)
  command(a,'revoke',public_key=browser_key)
  until(lambda:not any(p['status']=='Live' for p in sync()['peers']),'signed revocation disconnects the browser and removes bundle access')
  assert not evaluate("host.inventorySync().then(state=>state.peers.some(p=>p.active&&p.gitBundles))")



 finally:
  for process in reversed(processes):
   process.terminate()
  for process in reversed(processes):
   try:process.wait(timeout=5)
   except subprocess.TimeoutExpired:process.kill()

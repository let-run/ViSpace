#!/usr/bin/env python3
"""Protocol-7 browser/native live sync in an isolated Chrome profile and temporary databases.
Build first: cargo build --manifest-path Cargo.toml --example browser_sync_peer
            ./script/web.sh --build-only
"""
import base64, hashlib, json, pathlib, socket, sys, subprocess, tempfile, time, urllib.request, uuid, websocket
root=pathlib.Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix='inventory-sync-test-') as temp:
 temp=pathlib.Path(temp);processes=[]
 try:
  def peer(name):
   folder=temp/name;folder.mkdir();p=subprocess.Popen([str(root/'target/debug/examples/browser_sync_peer'),str(folder)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True);processes.append(p);return p,json.loads(p.stdout.readline())['url'],folder
  a,url_a,folder_a=peer('a');b,url_b,folder_b=peer('b')
  def command(p,op,**args):
   p.stdin.write(json.dumps(dict(op=op,**args))+'\n');p.stdin.flush();r=json.loads(p.stdout.readline());
   if 'error' in r:raise RuntimeError(r['error'])
   return r['value']
  png=base64.b64decode('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=');pdf=b'%PDF-1.7 native fixture'
  image=command(a,'asset',path='native/photo.png',bytes=list(png));sheet=command(a,'asset',path='native/sheet.pdf',bytes=list(pdf))
  data=command(a,'get')['inventory'];data['parts']=[dict(id=str(uuid.uuid4()).upper(),barcode=str(uuid.uuid4()).upper(),number='SYNC-1',stock=5,description='Common description',image=dict(originalPath=image,previewPath=image,thumbnailPath=image),datasheet=sheet,notes=dict(text='line one\nline two\n',markdownSource='line one\nline two\n'))];command(a,'set',inventory=data)
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
    command(native,'authorize',public_key=public)
    evaluate(f"host.inventorySync('authorize',{{public_key:{json.dumps(info['public_key'])}}})")
   return result
  def edit(script):
   # The synchronous Rust editor deliberately pauses while an async projection is adopted.
   return evaluate("(async()=>{for(let i=0;i<400;i++){try{const d=JSON.parse(wasm.export_inventory());"+script+";wasm.import_inventory(JSON.stringify(d));return true;}catch(e){if(!String(e).includes('Sync is applying'))throw e;await new Promise(r=>setTimeout(r,25));}}throw Error('Sync never released the editor');})()")
  sync('connect',url=url_a);sync('connect',url=url_b)
  until(lambda:len(command(b,'get')['inventory']['parts'])==1 and evaluate('JSON.parse(wasm.export_inventory()).parts.length===1'),'native → browser → native relay')
  assert (folder_b/'Attachments/native/photo.png').read_bytes()==png
  assert (folder_b/'Attachments/native/sheet.pdf').read_bytes()==pdf
  assert evaluate("JSON.parse(wasm.export_inventory()).parts[0].datasheet.startsWith('data:application/pdf;base64,')")
  print('PASS native images and PDFs arrive before their events with matching relative paths',flush=True)
  until(lambda:len(sync()['topology']['edges'])==2 and len(command(a,'get')['topology']['edges'])==2 and len(command(b,'get')['topology']['edges'])==2,'browser relays topology announcements between native peers')
  assert len(command(a,'get')['topology']['nodes'])==3
  assert any(not n['direct'] and not n['local'] for n in command(a,'get')['topology']['nodes'])
  if len(sys.argv)>1:
   height=evaluate('innerHeight')
   for kind in ['mousePressed','mouseReleased']:cdp('Input.dispatchMouseEvent',dict(type=kind,x=45,y=height-18,button='left',clickCount=1))
   time.sleep(.8);capture=cdp('Page.captureScreenshot',dict(format='png'));pathlib.Path(sys.argv[1]).write_bytes(base64.b64decode(capture['data']))

  edit("d.parts[0].description='Live browser edit'")
  until(lambda:command(a,'get')['inventory']['parts'][0]['description']=='Live browser edit' and command(b,'get')['inventory']['parts'][0]['description']=='Live browser edit','browser edits stream live to multiple peers')
  assert command(a,'get')['inventory']['parts'][0]['datasheet']==sheet
  assert command(a,'get')['inventory']['parts'][0]['image']['originalPath']==image
  print('PASS unrelated browser edits preserve imported attachment paths',flush=True)
  for p in sync()['peers']:sync('disconnect',peer=p['id'])
  # Diverge the same field offline, then resolve through the actual browser window.
  data=command(a,'get')['inventory'];data['parts'][0]['description']='Their offline edit';command(a,'set',inventory=data)
  edit("d.parts[0].description='Our offline edit'")
  evaluate('host.backupInventorySqlite().then(()=>true)')
  applied_before_review = sync()['appliedHead']
  assert applied_before_review
  sync('connect',url=url_a)
  until(lambda:len(sync()['reviews'])>0,'concurrent edits produce a durable comparison')
  assert sync()['appliedHead'] == applied_before_review
  assert evaluate("JSON.parse(wasm.export_inventory()).parts[0].description==='Our offline edit'")
  assert command(a,'get')['inventory']['parts'][0]['description']=='Their offline edit'
  evaluate("window.dispatchEvent(new Event('inventory-sync-open'));document.querySelector('#sync-tab-connect').click();true")
  until(lambda:evaluate("!!document.querySelector('dialog textarea')"),'Sync window shows comparison and editor')
  evaluate("(()=>{const e=document.querySelector('dialog textarea');e.value=JSON.stringify('Resolved in browser');e.dispatchEvent(new Event('input'));return true;})()")
  time.sleep(.4)
  review=sync()['reviews'][0];assert json.loads(review['draft'])=='Resolved in browser'
  evaluate("[...document.querySelectorAll('dialog button')].find(b=>b.textContent==='Resolve').click();true")
  until(lambda:command(a,'get')['inventory']['parts'][0]['description']=='Resolved in browser' and not sync()['reviews'],'browser review choice converges without publishing unresolved local edits')
  assert sync()['appliedHead'] != applied_before_review
  sync('connect',url=url_b)
  until(lambda:command(b,'get')['inventory']['parts'][0]['description']=='Resolved in browser','merge decision propagates to another peer')
  # New browser-owned files must use their own stable paths on native peers.
  browser_pdf=b'%PDF-1.7 browser fixture';pdf_uri='data:application/pdf;base64,'+base64.b64encode(browser_pdf).decode()
  edit("d.parts[0].datasheet="+json.dumps(pdf_uri)+";d.parts[0].notes.images.diagram='data:image/png;base64,"+base64.b64encode(png).decode()+"'")
  until(lambda:command(a,'get')['inventory']['parts'][0]['datasheet'].endswith(hashlib.sha256(browser_pdf).hexdigest()+'.pdf') and command(b,'get')['inventory']['parts'][0]['datasheet'].endswith(hashlib.sha256(browser_pdf).hexdigest()+'.pdf'),'browser attachment events reach both native peers')
  for peer_process in (a,b):
   part=command(peer_process,'get')['inventory']['parts'][0];assert pathlib.Path(part['datasheet']).read_bytes()==browser_pdf;assert pathlib.Path(part['notes']['images']['diagram']).read_bytes()==png
  print('PASS browser PDF/image bytes are transferred separately with matching relative paths',flush=True)
  until(lambda:command(a,'get')['head']['hash']==command(b,'get')['head']['hash']==sync()['head']['hash'],'all peers share the same HEAD')
  assert command(a,'get')['appliedHead'] == command(b,'get')['appliedHead'] == sync()['appliedHead']
  # Markdown conflicts keep a durable draft and refuse unresolved conflict markers.
  for p in sync()['peers']:sync('disconnect',peer=p['id'])
  data=command(a,'get')['inventory'];data['parts'][0]['notes']['text']='Their line\nline two\n';data['parts'][0]['notes']['markdownSource']='Their line\nline two\n';command(a,'set',inventory=data)
  edit("d.parts[0].notes.text='Our line\\nline two\\n';d.parts[0].notes.markdownSource=d.parts[0].notes.text")
  evaluate('host.backupInventorySqlite().then(()=>true)');sync('connect',url=url_a)
  until(lambda:any(r['markdown'] for r in sync()['reviews']),'Markdown edits produce a three-way merge draft')
  review=next(r for r in sync()['reviews'] if r['markdown']);assert '<<<<<<< OURS' in review['draft']
  try:sync('resolve',conflict=review['id'],value=json.loads(review['draft']))
  except RuntimeError as e:assert 'Remove the conflict markers' in str(e)
  else:raise RuntimeError('Unresolved Markdown markers were accepted')
  draft=json.loads(review['draft']);draft['text']=draft['markdownSource']='Merged line\nline two\n';sync('draft',conflict=review['id'],text=json.dumps(draft))
  before_reload_id=sync()['applicationId'];evaluate('location.reload();true')
  until(lambda:evaluate("!document.querySelector('#loading') && !!globalThis.__inventorySqliteBridge"),'browser reopens a pending comparison')
  evaluate("(async()=>{window.host=await import('./host.js');window.wasm=await import('./pkg/vispace.js');return true;})()")
  pending=sync();assert pending['applicationId']==before_reload_id;assert json.loads(pending['reviews'][0]['draft'])['markdownSource']=='Merged line\nline two\n'
  sync('connect',url=url_a)
  until(lambda:any(p['status']=='Live' for p in sync()['peers']),'browser reconnects with unresolved history held locally')
  evaluate("window.dispatchEvent(new Event('inventory-sync-open'));document.querySelector('#sync-tab-connect').click();true")
  until(lambda:evaluate("document.querySelector('dialog textarea')?.value==='Merged line\\nline two\\n'"),'Markdown draft survives reload in the editor')
  evaluate("[...document.querySelectorAll('dialog button')].find(b=>b.textContent==='Resolve').click();true")
  until(lambda:not sync()['reviews'] and command(a,'get')['inventory']['parts'][0]['notes']['text']=='Merged line\nline two\n','Markdown resolution reaches the native peer')
  sync('connect',url=url_b)
  until(lambda:command(a,'get')['head']['hash']==command(b,'get')['head']['hash']==sync()['head']['hash'],'resolved Markdown propagates across the mesh')
  identity=sync()['applicationId'];old_head=sync()['head']['hash']
  evaluate("location.reload();true")
  until(lambda:evaluate("!document.querySelector('#loading') && !!globalThis.__inventorySqliteBridge"),'browser reopens persisted SQLite history')
  evaluate("(async()=>{window.host=await import('./host.js');window.wasm=await import('./pkg/vispace.js');return true;})()")
  assert sync()['applicationId']==identity;assert sync()['head']['hash']==old_head
  sync('connect',url=url_a)
  until(lambda:any(p['status']=='Live' for p in sync()['peers']),'browser reconnects using persisted progress')
  assert sync()['head']['hash']==old_head;assert evaluate('JSON.parse(wasm.export_inventory()).parts[0].stock')==5
  print('PASS reconnect preserves identity and stock without duplicate events',flush=True)
 finally:
  for p in reversed(processes):
   p.terminate()
  for p in reversed(processes):
   try:p.wait(timeout=10)
   except subprocess.TimeoutExpired:p.kill()

import {inventorySync} from './host.js';
import {nodeId} from './sync-client.js';

const display = value => typeof value === 'string' ? value : JSON.stringify(value, null, 2);
const markdown = value => value?.markdownSource ?? value?.text ?? '';
const meshIcon = '<svg aria-hidden="true" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><path d="M10.7 7.1 6.1 15.7m7.2-8.6 4.6 8.6M8.2 18.2h7.6"/><circle cx="12" cy="4.6" r="2.8"/><circle cx="4.4" cy="18.2" r="2.8"/><circle cx="19.6" cy="18.2" r="2.8"/></svg>';

export function installSyncUI() {
 const dialog = document.createElement('dialog');
 dialog.className = 'inventory-sync-dialog';
 dialog.setAttribute('aria-labelledby', 'sync-title');
 dialog.innerHTML = `<style>
 .inventory-sync-dialog{box-sizing:border-box;background:#202028;color:#e0dfe5;border:1px solid #414148;border-radius:10px;width:min(726px,calc(100vw - 28px));padding:12px 22px}
 .inventory-sync-dialog::backdrop{background:#0007}
 .inventory-sync-dialog [hidden]{display:none!important}
 .inventory-sync-dialog .sync-titlebar,.inventory-sync-dialog .sync-footer{display:flex;align-items:center;justify-content:space-between;gap:8px}
 .inventory-sync-dialog .sync-titlebar svg{color:#f4d95c}
 .inventory-sync-dialog #sync-title{font-size:18px;margin:0;overflow-wrap:anywhere}
 .inventory-sync-dialog .sync-tabs{display:flex;gap:6px;margin:6px 0;padding-bottom:6px;border-bottom:1px solid #414148}
 .inventory-sync-dialog button{padding:4px 8px}
 .inventory-sync-dialog .sync-tabs button[aria-selected="false"]{background:transparent;border-color:transparent}
 .inventory-sync-dialog .sync-tabs button[aria-selected="true"]{background:#4b406d;color:#eee}
 .inventory-sync-dialog .sync-body{height:min(450px,55vh);overflow:auto}
 .inventory-sync-dialog .sync-footer{justify-content:flex-end;border-top:1px solid #414148;padding-top:6px;margin-top:6px}
 .inventory-sync-dialog table{width:calc(100% - 8px);margin:8px 4px;border-collapse:collapse;text-align:left}
 .inventory-sync-dialog th,.inventory-sync-dialog td{padding:4px 6px;white-space:nowrap}
 .inventory-sync-dialog th{color:#f4d95c}
 .inventory-sync-dialog td:first-child{width:100%;font-family:monospace}
 .inventory-sync-dialog tr:nth-child(odd) td{background:#252529}
 .inventory-sync-dialog td button+button{margin-left:6px}
 .inventory-sync-dialog .sync-icon-action{padding-inline:4px}
 .inventory-sync-dialog .sync-icon-action svg{vertical-align:middle}
 .inventory-sync-dialog input,.inventory-sync-dialog textarea{box-sizing:border-box;background:#15151b;color:#eee;border:1px solid #514667;border-radius:4px;padding:6px}
 .inventory-sync-dialog form{display:flex;gap:8px}.inventory-sync-dialog input{min-width:0;flex:1}
 .inventory-sync-dialog textarea{width:100%;min-height:160px;font:13px monospace}
 .inventory-sync-dialog pre{white-space:pre-wrap;overflow-wrap:anywhere;background:#15151b;padding:10px;max-height:220px;overflow:auto}
 .inventory-sync-dialog .comparisons{display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:10px}
 .inventory-sync-dialog .actions{display:flex;gap:8px;margin:8px 0}
 .inventory-sync-dialog li{margin:8px 0;overflow-wrap:anywhere}
 .inventory-sync-dialog #sync-error{color:#ffb4ac;margin:6px 0}
 @media(max-width:550px){.inventory-sync-dialog{padding-inline:10px}.inventory-sync-dialog .comparisons{grid-template-columns:1fr}}
 </style>
 <div class="sync-titlebar"><h2 id="sync-title">Sync</h2>${meshIcon}</div>
 <div class="sync-tabs" role="tablist" aria-label="Sync">
  <button id="sync-tab-discover" type="button" role="tab" aria-controls="sync-discover">Discover</button>
  <button id="sync-tab-connect" type="button" role="tab" aria-controls="sync-connect">Connect</button>
 </div>
 <div class="sync-body">
  <section id="sync-discover" role="tabpanel" aria-labelledby="sync-tab-discover">
   <small>Use “Find nearby sync nodes” in the Chrome extension to discover desktop nodes.</small>
   <p id="sync-empty">No nearby or approved nodes yet. You can enter an address in Connect.</p>
   <table id="sync-table" hidden><thead><tr><th scope="col">Node</th><th scope="col">Status</th><th scope="col">Pairing</th><th scope="col">Actions</th></tr></thead><tbody id="sync-nearby"></tbody></table>
  </section>
  <section id="sync-connect" role="tabpanel" aria-labelledby="sync-tab-connect" hidden>
   <form><input id="sync-url" aria-label="Peer address" placeholder="wss://localhost:8824" required><button>Connect</button></form>
   <ul id="sync-peers"></ul>
   <details><summary>Signed network action log</summary><div id="sync-security"></div></details>
   <div id="sync-warnings"></div><section id="sync-pending"></section><section id="sync-review"></section>
  </section>
 </div>
 <p id="sync-error" role="alert"></p>
 <div class="sync-footer"><button id="sync-close" type="button">Done</button></div>`;
 document.body.append(dialog);
 let state, reviewId = null, draftTimer, nearby = [], discoveredAt = 0, rowsVersion;
 let revokingId = null;
 const el = id => dialog.querySelector('#sync-' + id);
 const error = err => { el('error').textContent = String(err.message || err); };
 const tabs = ['discover', 'connect'];
 function selectTab(selected, focus = false) {
  if (selected !== 'discover' && revokingId !== null) {
   revokingId = null;
   renderNodes();
  }
  for (const tab of tabs) {
   const button = el('tab-' + tab);
   button.setAttribute('aria-selected', String(tab === selected));
   button.tabIndex = tab === selected ? 0 : -1;
   el(tab).hidden = tab !== selected;
   if (tab === selected && focus) button.focus();
  }
 }
 for (const tab of tabs) {
  const button = el('tab-' + tab);
  button.onclick = () => selectTab(tab);
  button.onkeydown = event => {
   if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
   event.preventDefault();
   selectTab(event.key === 'Home' ? tabs[0] : event.key === 'End' ? tabs[1] : tabs.find(other => other !== tab), true);
  };
 }
 selectTab('discover');
 el('close').onclick = () => dialog.close();
 dialog.addEventListener('close', () => { revokingId = null; });

 async function operation(op, args) {
  try {
   state = await inventorySync(op, args);
   el('error').textContent = '';
   render();
  } catch (err) { error(err); }
 }
 function open() {
  if (!dialog.open) dialog.showModal();
  refresh();
 }
 globalThis.addEventListener('inventory-sync-open', open);
 dialog.querySelector('form').onsubmit = event => {
  event.preventDefault();
  operation('connect', {url: el('url').value.trim()});
 };
 globalThis.addEventListener('message', event => {
  if (event.source !== window || event.origin !== location.origin
      || event.data?.channel !== 'vispace.discovery.v1' || !Array.isArray(event.data.nodes)) return;
  nearby = event.data.nodes.slice(0, 128).filter(node => {
   if (!nodeId.test(node?.node_id ?? '')) return false;
   try {
    const url = new URL(node.address);
    return url.protocol === 'wss:' && url.hostname === node.node_id + '.vispace.local'
        && !url.username && !url.password && url.pathname === '/' && !url.search && !url.hash;
   } catch { return false; }
  });
  discoveredAt = Date.now();
  selectTab('discover');
  open();
 });

 async function refresh() {
  if (!dialog.open) return;
  try {
   state = await inventorySync();
   render();
  } catch (err) { error(err); }
 }
 function action(label, callback) {
  const button = document.createElement('button');
  button.type = 'button';
  button.textContent = label;
  button.onclick = callback;
  return button;
 }
 function iconAction(label, path, callback) {
  const button = action('', callback);
  button.className = 'sync-icon-action';
  button.setAttribute('aria-label', label);
  button.title = label;
  button.innerHTML = `<svg aria-hidden="true" width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="${path}"/></svg>`;
  return button;
 }
 function renderNodes(focusAction) {
  const known = new Map((state.security?.pending ?? []).map(node => [node.id, node]));
  const now = Date.now();
  const recent = now - discoveredAt < 24 * 60 * 60 * 1000 ? nearby : [];
  const addresses = new Map(recent.map(node => [node.node_id, node.address]));
  const active = state.peers.filter(peer => peer.active);
  const online = new Set((state.topology?.nodes ?? []).filter(node => node.reachable || node.direct).map(node => node.id));
  const ids = [...new Set([...known.keys(), ...addresses.keys()])]
    .filter(id => {
     if (id === state.applicationId) return false;
     const member = known.get(id);
     return member?.expires_at == null || member.expires_at > now || (!member.revoked
       && (addresses.has(id) || active.some(peer => peer.pairingId === id || peer.peerId === id)));
    })
    .sort((a, b) => Number(!!known.get(b)?.trusted) - Number(!!known.get(a)?.trusted) || a.localeCompare(b));
  const rows = ids.map(id => {
   const member = known.get(id);
   const peer = active.find(peer => (peer.peerId ?? peer.pairingId) === id || peer.url === addresses.get(id));
   const address = addresses.get(id);
   return {
    id, member, address, pairing: !!peer?.pairingId && !peer.peerId, canConnect: !!address && !member?.revoked && active.length === 0,
    status: peer?.status === 'Live' ? 'Connected' : peer ? peer.status
      : address || online.has(id) ? 'Online' : 'Offline',
   };
  });
  if (!rows.some(row => row.id === revokingId && row.member?.trusted && !row.member.revoked)) revokingId = null;
  const version = JSON.stringify([rows, revokingId]);
  if (version === rowsVersion) return;
  rowsVersion = version;
  el('empty').hidden = rows.length > 0;
  el('table').hidden = rows.length === 0;
  const tbody = el('nearby');
  tbody.replaceChildren();
  for (const {id, member, address, canConnect, status, pairing} of rows) {
   const row = tbody.insertRow();
   const name = row.insertCell();
   name.textContent = id;
   if (pairing) name.style.color = "#47b391";
   name.title = address ?? id;
   const statusCell = row.insertCell();
   statusCell.textContent = status;
   if (status === 'Online' || status === 'Connected') statusCell.style.color = '#47b391';
   if (status === 'Offline') statusCell.style.color = '#b0adc2';
   row.insertCell().textContent = member?.trusted ? 'Approved' : member?.revoked ? 'Revoked' : 'Unpaired';
   const controls = row.insertCell();
   const connect = action('Connect', () => operation('connect', {url: address, expected_id: id}));
   connect.disabled = !canConnect;
   controls.append(connect);
   if (member?.trusted && !member.revoked) {
    if (revokingId === id) {
     const confirm = iconAction('Confirm revoke', 'M2.5 8 6.5 12 13.5 4', async event => {
      if (event.detail > 1) return;
      confirm.disabled = true;
      await operation('revoke', {public_key: member.public_key});
      confirm.disabled = false;
     });
     const cancel = iconAction('Cancel revoke', 'M4 4l8 8m0-8-8 8', () => {
      revokingId = null;
      renderNodes(id);
     });
     controls.append(confirm, cancel);
     if (focusAction === id) cancel.focus();
    } else {
     const revoke = action('Revoke', () => {
      revokingId = id;
      renderNodes(id);
     });
     revoke.title = 'Revoke sync access';
     controls.append(revoke);
     if (focusAction === id) revoke.focus();
    }
   } else if (member && !member.revoked) {
    const approve = action('Approve', () => operation('authorize', {public_key: member.public_key}));
    approve.title = 'Verify the node ID before approving';
    controls.append(approve);
   }
  }
 }
 function render() {
  el('title').textContent = 'Sync · ' + state.applicationId;
  renderNodes();
  el('peers').replaceChildren();
  for (const peer of state.peers) {
   const li = document.createElement('li');
   const details = peer.status === 'Live'
     ? (peer.gitBundles ? ' · Git bundles ready' : ' · Git transfer unavailable')
     : peer.status === 'Waiting for approval' ? ' · Verify and approve the node in Discover' : '';
   li.append(peer.url + ' · ' + peer.status + details + ' ');
   if (peer.active) li.append(action('Disconnect', () => operation('disconnect', {peer: peer.id})));
   if (/diverged|same sync mode|Partition sync/.test(peer.status)) {
    li.append(action('Review branches', () => operation('bridge_open', {url: peer.url})));
   }
   el('peers').append(li);
  }
  const security = el('security');
  security.replaceChildren();
  for (const entry of state.security?.actions ?? []) {
   const row = document.createElement('p');
   row.textContent = (entry.revoked ? 'Revoked ' : 'Approved ') + entry.subject + ' · signed by ' + entry.issuer;
   security.append(row);
  }
  el('warnings').textContent=state.warnings.map(w=>w.message).join('\n');
  const pending=state.pending,panel=el('pending');panel.replaceChildren();
  if(state.component&&state.queued){const count=document.createElement('small');count.textContent=`${state.queued} edits awaiting approval or review`;panel.append(count);}
  if(pending){
   const {outcome}=pending;const text=document.createElement('p');text.textContent=outcome.result==='stale'?(pending.ready?'Review your draft against the latest shared values.':'Receiving the latest shared values…'):outcome.result==='invalid'?String(outcome.detail):'Your edit is queued for approval.';panel.append(text);
   const retry=outcome.result==='unavailable',button=document.createElement('button');button.textContent=retry?'Retry draft':'Discard draft';
   button.onclick=async()=>{try{state=await inventorySync(retry?'retry_proposal':'discard_proposal');el('error').textContent='';render();}catch(e){error(e);await refresh();}};panel.append(button);
  }
  const bridge=state.bridge;
  if(bridge){
   const heading=document.createElement('p');heading.textContent='Review connection to '+bridge.address;panel.append(heading);
   const actions=[['Refresh comparison','bridge_refresh'],['Cancel','bridge_cancel']];
   if(!bridge.reviews.length)actions.unshift(['Apply merge and connect','bridge_finish']);
   for(const [label,op] of actions){
    const button=document.createElement('button');button.textContent=label;
    button.onclick=async()=>{try{state=await inventorySync(op);el('error').textContent='';render();}catch(e){error(e);await refresh();}};
    panel.append(button);
   }
  }
  const proposal=pending?.ready?pending.comparisons.filter(r=>!Object.hasOwn(pending.choices,r.id)):[],next=bridge?.reviews[0]??proposal[0]??state.reviews[0];if(next?.id===reviewId)return;reviewId=next?.id??null;const area=el('review');area.replaceChildren();if(!next)return;
  const title=document.createElement('h3');title.textContent=`Compare ${next.entity} · ${next.field} (${bridge?.reviews.length||proposal.length||state.reviews.length} remaining) · ${next.key}`;area.append(title);const comparison=document.createElement('div');comparison.className='comparisons';for(const[key,label]of [['base','Common version'],['ours','Our version'],['theirs','Their version']]){const col=document.createElement('div'),heading=document.createElement('strong'),pre=document.createElement('pre');heading.textContent=label;pre.textContent=next.markdown?markdown(next[key]):display(next[key]);col.append(heading,pre);comparison.append(col);}area.append(comparison);
  const controls=document.createElement('div');controls.className='actions';const editor=document.createElement('textarea');editor.setAttribute('aria-label',next.markdown?'Merged Markdown':'Merged value as JSON');let value,rawDraft=false;try{value=next.draft===undefined?next.ours:JSON.parse(next.draft);}catch{value=next.ours;rawDraft=true;}editor.value=rawDraft?next.draft:next.markdown?markdown(value):JSON.stringify(value,null,2);
  const preview=document.createElement('pre');preview.setAttribute('aria-label','Markdown text preview');preview.hidden=!next.markdown;preview.textContent=editor.value;
  const saveDraft=()=>{if(bridge||proposal.length)return;clearTimeout(draftTimer);const id=next.id;draftTimer=setTimeout(()=>{try{let v=next.markdown?{...value,markdownSource:editor.value,text:editor.value,rtf:'',visualTokens:undefined,visualHTML:undefined,latexSource:undefined}:JSON.parse(editor.value);inventorySync('draft',{conflict:id,text:JSON.stringify(v)}).catch(error);}catch{inventorySync('draft',{conflict:id,text:editor.value}).catch(error);}},250);};
  for(const[key,label]of [['ours','Use ours'],['theirs','Use theirs']]){const b=document.createElement('button');b.textContent=label;b.onclick=()=>{value=next[key];editor.value=next.markdown?markdown(value):JSON.stringify(value,null,2);preview.textContent=editor.value;saveDraft();};controls.append(b);}
  const resolve=document.createElement('button');resolve.textContent='Resolve';resolve.onclick=async()=>{clearTimeout(draftTimer);try{const selected=next.markdown?{...value,markdownSource:editor.value,text:editor.value,rtf:'',visualTokens:undefined,visualHTML:undefined,latexSource:undefined}:JSON.parse(editor.value);state=await inventorySync(bridge?.reviews[0]?.id===next.id?'bridge_resolve':'resolve',{conflict:next.id,value:selected});el('error').textContent='';render();}catch(e){error(e);await refresh();}};controls.append(resolve);editor.oninput=()=>{preview.textContent=editor.value;saveDraft();};area.append(controls,editor,preview);
 }
 globalThis.addEventListener('inventory-sync-changed',refresh);
 setInterval(() => { if (dialog.open && state) renderNodes(); }, 60_000);
}

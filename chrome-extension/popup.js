const $ = id => document.getElementById(id);
const keys = ['number', 'manufacturer', 'supplier', 'spn', 'description', 'package', 'value', 'tolerance', 'datasheet', 'price'];
const titles = ['Part number', 'Manufacturer', 'Supplier', 'Supplier part number', 'Description', 'Package', 'Value', 'Tolerance', 'Datasheet URL', 'Supplier unit price'];
let route = null, loaded = false, attempt = null, busy = true, destination = 'http';
let projects = [], availableTabs = [];
const projectKey = name => name.trim().toLowerCase();
const destinationTabs = ['web', 'http'];

function updateControls() {
  const locked = Boolean(attempt && !attempt.receipt);
  for (const id of ['address', 'web-url', 'web-tab', 'find-tabs', 'discover-sync']) $(id).disabled = locked || busy;
  for (const mode of destinationTabs) $('tab-' + mode).disabled = locked || busy;
  $('connect').disabled = busy;
  $('scrape').disabled = busy;
  $('submit').disabled = busy || !loaded || Boolean(attempt?.receipt);
  for (const input of $('purchase').querySelectorAll('input')) input.disabled = Boolean(attempt) || busy;
  if (!attempt && !busy) {
    for (const row of document.querySelectorAll('.project')) row.children[2].disabled = !row.children[0].checked;
  }
}

function resetProjects() {
  loaded = false;
  route = null;
  projects = [];
  $('projects').textContent = 'Connect to load projects.';
  updateControls();
}

function selectDestination(mode) {
  destination = mode;
  for (const name of destinationTabs) {
    const selected = mode === name;
    $('tab-' + name).setAttribute('aria-selected', String(selected));
    $('tab-' + name).tabIndex = selected ? 0 : -1;
    $('panel-' + name).hidden = !selected;
  }
  resetProjects();
}
for (const mode of destinationTabs) {
  const button = $('tab-' + mode);
  button.onclick = () => {
    if (destination === mode) return;
    selectDestination(mode);
    status('');
    chrome.storage.local.set({destination}).catch(error => status(error.message));
  };
  button.onkeydown = event => {
    if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
    event.preventDefault();
    const next = event.key === 'Home' ? 'web' : event.key === 'End' ? 'http' : mode === 'web' ? 'http' : 'web';
    $('tab-' + next).click();
    $('tab-' + next).focus();
  };
}

function webTab(){const value=Number($('web-tab').value);if(!value)throw Error('Open ViSpace in a browser tab, refresh app tabs, and select it.');return value;}
async function findTabs(){
 const current=$('web-tab').value;
 let origin='';try{origin=new URL($('web-url').value).origin;}catch{}
 const tabs=(await chrome.tabs.query({})).filter(tab=>{try{const url=new URL(tab.url);return ['http:','https:'].includes(url.protocol)&&(tab.title?.startsWith('ViSpace')||url.origin===origin);}catch{return false;}});
 availableTabs=tabs;
 $('web-tab').replaceChildren();
 for(const tab of tabs){const option=document.createElement('option');option.value=tab.id;option.textContent=`${tab.title} — ${tab.url}`;$('web-tab').append(option);}
 if(tabs.some(t=>String(t.id)===current))$('web-tab').value=current;
 if(!tabs.length){const option=document.createElement('option');option.value='';option.textContent='No app tab found';$('web-tab').append(option);}
 if ($('web-tab').value !== current && destination === 'web' && (!attempt || attempt.receipt)) resetProjects();
}

let scrapedPrices = '', singleUnitPrice = '', manualPrice = false;
function updatePrice() {
  if (!manualPrice) $('price').value = priceForQuantity(scrapedPrices, Number($('quantity').value), singleUnitPrice);
}
for (const [i, key] of keys.entries()) {
  const label = document.createElement('label'); label.textContent = titles[i];
  const input = document.createElement('input'); input.id = key; input.required = key === 'number';
  if (key === 'datasheet') input.type = 'url';
  label.append(input); $('fields').append(label);
}
$('quantity').addEventListener('input', updatePrice);
$('price').addEventListener('input', () => { manualPrice = true; });
const status = message => { $('status').textContent = message; };
$('discover-sync').onclick=async()=>{
 const button=$('discover-sync');button.disabled=true;
 try{
  const tab=availableTabs.find(t=>t.id===webTab());if(!tab)throw Error('Select the browser app tab first.');
  const origin=new URL(tab.url).origin;
  if(!await chrome.permissions.request({permissions:['nativeMessaging'],origins:[origin+'/*']}))throw Error('Discovery permission was not granted.');
  status('Looking for nearby sync nodes…');
  const result=await chrome.runtime.sendNativeMessage('run.let.vispace_discovery',{operation:'discover'});
  if(result?.error)throw Error(result.error);
  if(!Array.isArray(result?.nodes))throw Error('Invalid discovery response.');
  const [sent]=await chrome.scripting.executeScript({target:{tabId:tab.id},func:(nodes,expected)=>{
   if(location.origin!==expected||document.documentElement.dataset.bomInventoryBridge!=='1')return false;
   window.postMessage({channel:'vispace.discovery.v1',nodes},location.origin);return true;
  },args:[result.nodes,origin]});
  if(!sent?.result)throw Error('The selected tab is not a ready browser inventory.');
  await chrome.tabs.update(tab.id,{active:true});
  status(`Found ${result.nodes.length} nearby nodes. See Sync in the browser app.`);
 }catch(e){status(`${e.message} You can enter a WSS address manually in browser Sync.`);}finally{button.disabled=false;}
};
$('purchase').addEventListener('invalid', event => { status(`Check ${event.target.closest('label')?.firstChild?.textContent || 'the highlighted field'}: ${event.target.validationMessage}`); }, true);
function endpoint() {
  const address = $('address').value.trim();
  const message = 'Enter a desktop address such as wss://127.0.0.1:8824, without a path.';
  if (!/^(?:https?|wss):\/\//i.test(address)) throw Error(message);
  // Keep the HTTPS origin for Chrome permissions. Desktop requests convert it to WSS.
  const url = URL.parse(address.replace(/^(?:https?|wss):/i, 'https:'));
  if (!url || url.username || url.password || url.pathname !== '/' || url.search || url.hash || url.port === '0') throw Error(message);
  return url.origin;
}
function desktopAddress(origin) {
  return origin.replace(/^https?:/i, 'wss:');
}
async function permission(url) {
  const origin = new URL(url).origin + '/*';
  if (!await chrome.permissions.request({origins: [origin]})) throw new Error('Host access was not granted.');
}
function desktopRequest(operation, payload, address) {
  const url = new URL(address);
  url.protocol = 'wss:';
  url.pathname = '/extension';
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(url);
    const finish = (error, value) => {
      clearTimeout(timer);
      socket.close();
      if (error) reject(Error(error));
      else resolve(value);
    };
    const timer = setTimeout(() => finish('Desktop extension connection timed out.'), 12000);
    socket.onopen = () => socket.send(JSON.stringify({operation, payload}));
    socket.onmessage = event => {
      try {
        const response = JSON.parse(event.data);
        finish(response.error, response.value);
      } catch {
        finish('Invalid response from the desktop app.');
      }
    };
    socket.onerror = () => finish('Could not connect. Start the desktop extension listener.');
    socket.onclose = () => finish('Desktop app closed the connection before responding.');
  });
}
function showProjects(entries) {
  projects = entries;
  $('projects').replaceChildren();
  for (const project of projects) {
    const row = document.createElement('label'); row.className = 'project';
    const check = document.createElement('input'); check.type = 'checkbox'; check.dataset.project = project.name;
    const name = document.createElement('span'); name.textContent = project.name; name.title = project.description || '';
    const count = document.createElement('input'); count.type = 'number'; count.min = '1'; count.step = '1'; count.value = '1'; count.disabled = true; count.setAttribute('aria-label', `Quantity for ${project.name}`);
    check.addEventListener('change', () => { count.disabled = !check.checked; });
    row.append(check, name, count); $('projects').append(row);
  }
  if (!projects.length) $('projects').textContent = 'No projects yet. You can add an unallocated purchase.';
}

async function connect(requestPermission = true) {
  if (busy) return;
  const retry = attempt && !attempt.receipt ? attempt : null;
  busy = true;
  loaded = false;
  updateControls();
  try {
    let tabId = null, origin;
    if (destination === 'http') {
      origin = retry ? retry.base : endpoint();
    } else {
      tabId = retry ? retry.tabId : webTab();
      const tab = availableTabs.find(tab => tab.id === tabId);
      if (!tab) throw Error(retry ? 'The original browser tab is closed. Check Purchase before starting over.' : 'Refresh app tabs and choose the browser app.');
      origin = new URL(tab.url).origin;
      if (retry?.origin && origin !== retry.origin) throw Error('The original tab has changed address. Check Purchase before starting over.');
    }
    const origins = [origin + '/*'];
    if (requestPermission) {
      if (!await chrome.permissions.request({origins})) throw Error('Destination access was not granted.');
    } else if (!await chrome.permissions.contains({origins})) {
      throw Error('Connect to grant access to the selected app.');
    }
    const request = destination === 'http'
      ? operation => desktopRequest(operation, {}, origin)
      : operation => inventoryTabRequest(tabId, operation, {}, origin);
    const health = await request('health');
    if (health?.application !== 'bom-inventory' || health.protocol !== 1) throw Error('Selected app is not a compatible ViSpace destination.');
    const entries = retry ? retry.projects : await request('projects');
    if (!Array.isArray(entries)) throw Error('Unexpected project list.');
    if (!retry) {
      await chrome.storage.local.set({desktopAddress:$('address').value.trim(),destination,webURL:$('web-url').value,webTab:$('web-tab').value});
    }
    route = {destination, tabId, origin, base: destination === 'http' ? origin : null};
    if (retry && destination === 'web' && !retry.origin) {
      retry.origin = origin;
      await chrome.storage.local.set({pendingPurchase: attempt});
    }
    showProjects(entries);
    loaded = true;
    if (retry) restoreAttempt();
    status(retry ? 'Previous purchase restored. You can retry to the original app.' : 'Connected. Projects loaded.');
  } catch (error) {
    if (!retry) resetProjects();
    status(`Connection failed: ${error.message}`);
  } finally {
    busy = false;
    updateControls();
  }
}

$('connect').addEventListener('click', () => connect());
$('find-tabs').addEventListener('click', async () => {
  busy = true;
  updateControls();
  try { await findTabs(); }
  catch (error) { status(error.message); }
  finally { busy = false; updateControls(); }
});
for (const id of ['address', 'web-tab', 'web-url']) $(id).addEventListener('input', resetProjects);
$('scrape').addEventListener('click', async () => {
  busy = true;
  updateControls();
  try {
    const [tab] = await chrome.tabs.query({active: true, currentWindow: true});
    const host = new URL(tab.url).hostname;
    const isLCSC = /(^|\.)lcsc\.com$/i.test(host);
    const isDigiKey = /(^|\.)digikey\.(?:com|ca|co\.uk|de|fr|jp|cn|hk|com\.au|co\.nz|in|sg|tw|kr|nl|it|es|se|dk|no|fi|at|be|ch|ie|co\.il|co\.za|com\.mx)$/i.test(host);
    const isMouser = /(^|\.)mouser\.(?:com|cn|jp|kr|co\.uk|de|fr|it|es|in|ca|com\.au)$/i.test(host);
    if (!isLCSC && !isDigiKey && !isMouser) throw new Error('Open an LCSC, DigiKey, or Mouser product detail page first.');
    const scraper = isMouser ? scrapeMouser : isDigiKey ? scrapeDigiKey : scrapeLCSC;
    const [result] = await chrome.scripting.executeScript({target: {tabId: tab.id}, func: scraper});
    if (!result?.result) throw new Error('Could not read this page.');
    for (const key of [...keys, 'image']) $(key).value = result.result[key] || '';
    scrapedPrices = result.result.price || ''; singleUnitPrice = result.result.singleUnitPrice || ''; manualPrice = false; updatePrice();
    await chrome.storage.local.remove('pendingPurchase');
    if (attempt && !attempt.receipt) resetProjects();
    attempt = null;
    $('submit').textContent = 'Add to Purchase';
    $('preview').hidden = true;
    status('Product read. Review the fields, photo URL, datasheet, and quantities before adding.');
  } catch (error) { status(error.message); }
  finally { busy = false; updateControls(); }
});
async function imageBase64(url) {
  const parsed = new URL(url);
  if (!['http:', 'https:'].includes(parsed.protocol)) throw new Error('Image must use HTTP or HTTPS.');
  const response = await fetch(url, {signal: AbortSignal.timeout(15000), credentials: 'omit'});
  if (!response.ok) throw new Error(`Image download failed (${response.status}).`);
  const blob = await response.blob();
  if (!blob.type.startsWith('image/') || blob.size > 10_000_000) throw new Error('Image must be an image smaller than 10 MB.');
  const bitmap = await createImageBitmap(blob);
  try {
    const scale = Math.min(1, 800 / Math.max(bitmap.width, bitmap.height));
    const canvas = document.createElement('canvas'); canvas.width = Math.max(1, Math.round(bitmap.width * scale)); canvas.height = Math.max(1, Math.round(bitmap.height * scale));
    const ctx = canvas.getContext('2d'); ctx.fillStyle = 'white'; ctx.fillRect(0, 0, canvas.width, canvas.height); ctx.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
    const data = canvas.toDataURL('image/jpeg', .85);
    if (data.length > 800000) throw new Error('Image is too large after resizing.');
    $('preview').src = data; $('preview').hidden = false;
    return data.split(',')[1];
  } finally { bitmap.close(); }
}
function restoreAttempt(){
  for(const key of keys)$(key).value=key==='price'?Object.values(attempt.payload.part.customFields||{})[0]||'':attempt.payload.part[key]||'';
  $('quantity').value=attempt.payload.quantity;
  for(const row of document.querySelectorAll('.project')){
    const assignment=attempt.payload.assignments.find(a=>projectKey(a.project)===projectKey(row.children[0].dataset.project));
    row.children[0].checked=!!assignment;if(assignment)row.children[2].value=assignment.quantity;
  }
  if(attempt.payload.imageBase64){$('preview').src='data:image/jpeg;base64,'+attempt.payload.imageBase64;$('preview').hidden=false;}
  $('submit').textContent = attempt.receipt ? 'Added' : 'Retry purchase';
  updateControls();
}
$('purchase').addEventListener('submit',async event=>{
 event.preventDefault();
 if (!loaded || busy || attempt?.receipt) return;
 busy = true;
 updateControls();
 let sending=false;
 try{
  if(!attempt){
    status('Preparing purchase… Keep this popup open until finished.');
    const imageURL=$('image').value.trim();if(imageURL)await permission(imageURL);
    const quantity=Number($('quantity').value);if(!Number.isSafeInteger(quantity)||quantity<1)throw Error('Purchase quantity must be a positive integer.');
    const assignments=[...document.querySelectorAll('.project')].filter(row=>row.children[0].checked).map(row=>({project:row.children[0].dataset.project,quantity:Number(row.children[2].value)}));
    if(assignments.some(a=>!Number.isSafeInteger(a.quantity)||a.quantity<1)||assignments.reduce((n,a)=>n+a.quantity,0)>quantity)throw Error('Project quantities must be positive integers and their total cannot exceed purchase quantity.');
    const part=Object.fromEntries(keys.filter(key=>key!=='price').map(key=>[key,$(key).value.trim()]));updatePrice();const price=$('price').value.trim();
    if(price)part.customFields={[`${part.supplier||'Supplier'} Price`]:price};if(!part.number)throw Error('Part number is required.');
    const payload={part,quantity,assignments,requestID:crypto.randomUUID()};if(imageURL)payload.imageBase64=await imageBase64(imageURL);
    const pending = {...route, payload, receipt: null, projects};
    await chrome.storage.local.set({pendingPurchase: pending});
    attempt = pending;
  }
  sending = true;
  status('Saving purchase… Keep this popup open until finished.');
  await deliverPurchase(attempt, pending => pending.destination === 'web'
    ? inventoryTabRequest(pending.tabId, 'purchase', pending.payload, pending.origin)
    : desktopRequest('purchase', pending.payload, pending.base),
  () => chrome.storage.local.set({pendingPurchase: attempt}));
  status('Purchase saved. Read another product to start a new purchase.');
 } catch (error) {
  status(`${error.message}${sending ? '\nCheck Purchase before retrying.' : '\nNothing was submitted.'}`);
 } finally {
  if (attempt) $('submit').textContent = attempt.receipt ? 'Added' : 'Retry purchase';
  busy = false;
  updateControls();
 }
});

updateControls();
(async () => {
 const saved = await chrome.storage.local.get({desktopAddress:null,address:'https://127.0.0.1',port:'8824',destination:'http',webURL:'https://localhost:8787',webTab:'',pendingPurchase:null});
 const legacy = URL.parse(desktopAddress(saved.address));
 if (legacy) legacy.port = saved.port === '8080' ? '8824' : saved.port;
 $('address').value = saved.desktopAddress ?? legacy?.origin ?? 'wss://127.0.0.1:8824';
 $('web-url').value = saved.webURL;
 attempt = restorePendingPurchase(saved.pendingPurchase, saved.destination);
 const retry = attempt && !attempt.receipt ? attempt : null;
 selectDestination(retry?.destination ?? (saved.destination === 'web' ? 'web' : 'http'));
 await findTabs();
 const tab = String(retry?.tabId ?? saved.webTab);
 if ([...$('web-tab').options].some(option => option.value === tab)) $('web-tab').value = tab;
 if (attempt) {
  if (retry?.destination === 'http' && retry.base) $('address').value = desktopAddress(retry.base);
  if (retry) showProjects(retry.projects);
  restoreAttempt();
  if (attempt.receipt) status('Previous purchase already saved. Read another product to begin.');
 }
 busy = false;
 if (retry) await connect(false);
 else updateControls();
})().catch(error => {
 busy = false;
 updateControls();
 status(error.message);
});

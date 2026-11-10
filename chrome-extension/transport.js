// Runs in the chosen app tab's isolated world. Only the narrow page bridge is exposed.
async function inventoryTabRequest(tabId,operation,payload={},origin) {
  const [result]=await chrome.scripting.executeScript({target:{tabId},func:async(operation,payload,origin)=>{
    if (origin && location.origin !== origin) throw Error('The selected app tab has changed address.');
    if(document.documentElement.dataset.bomInventoryBridge!=='1')throw Error('This tab is not a ready ViSpace browser app. Reload the app.');
    return new Promise((resolve,reject)=>{
      const id=crypto.randomUUID();
      const receive=event=>{
        const msg=event.data;
        if(event.source!==window||event.origin!==location.origin||msg?.channel!=='bom-inventory.supplier.v1'||msg.direction!=='response'||msg.id!==id)return;
        clearTimeout(timer);window.removeEventListener('message',receive);
        if(msg.error)reject(Error(msg.error));else resolve(msg.value);
      };
      const timer=setTimeout(()=>{window.removeEventListener('message',receive);reject(Error('Browser app did not confirm the request. Check Purchase before retrying.'));},30000);
      window.addEventListener('message',receive);
      window.postMessage({channel:'bom-inventory.supplier.v1',direction:'request',id,operation,payload:JSON.stringify(payload)},location.origin);
    });
  },args:[operation,payload,origin ?? null]});
  if(!result||result.result===undefined)throw Error('No response from the browser app.');
  return result.result;
}
// Keep the original request ID and route across retries. A legacy purchase
// with a receipt is complete even if its other destination never confirmed.
function restorePendingPurchase(saved, preferred) {
  if (!saved) return null;
  if (saved.destination === 'web' || saved.destination === 'http') return saved;
  const targets = (saved.targets ?? []).filter(target => target === 'http' || target === 'web');
  const destination = targets.find(target => saved.receipts?.[target])
    ?? (targets.includes(preferred) ? preferred : targets[0]);
  if (!destination) return null;
  return {
    destination,
    payload: saved.payload,
    receipt: saved.receipts?.[destination] ?? null,
    projects: saved.projectLists?.[destination] ?? [],
    base: saved.base,
    tabId: saved.tabId,
  };
}

async function deliverPurchase(attempt, send, onReceipt) {
  if (attempt.receipt) return;
  const response = await send(attempt);
  const id = response?.id || response?.part?.id;
  if (!id) throw Error('Destination did not confirm a saved purchase.');
  attempt.receipt = id;
  await onReceipt();
}

if (typeof module !== 'undefined') module.exports = {deliverPurchase, restorePendingPurchase};

// Promise RPC client. Save acknowledgements come only after files, chunks and SQLite commit.
export class InventoryStorage {
 constructor(worker=new Worker(new URL('./storage-worker.js',import.meta.url),{type:'module'})){
  this.worker=worker;this.pending=new Map();this.serial=0;this.state=null;
  worker.onmessage=({data})=>{if(data.event){this.onchange?.(data.event);return;}const request=this.pending.get(data.id);if(!request)return;this.pending.delete(data.id);data.error?request.reject(Error(data.error)):request.resolve(data.value);};
  worker.onerror=event=>{for(const p of this.pending.values())p.reject(Error(event.message||'Storage worker failed'));this.pending.clear();};
 }
 call(op,args={}){return new Promise((resolve,reject)=>{const id=++this.serial;this.pending.set(id,{resolve,reject});try{this.worker.postMessage({id,op,...args});}catch(error){this.pending.delete(id);reject(error);}});}
 async init(legacy,folder,demo=false){this.state=await this.call('init',{legacy,folder,demo});return this.state;}
 async save(next,expected=this.state.revision){this.state=await this.call('save',{next,expected});return this.state;}
 async bind(folder){this.state=await this.call('bind',{folder});return this.state;}
 async sync(op,args={}){this.state=await this.call(op,args);return this.state;}
 backup(){return this.call('backup');}
}

// Synchronous IO is confined to a dedicated worker. Only bounded chunks cross WASM.
const key=Symbol.for('vinventory.git.stream.io');
const CHUNK=64*1024;
function state(){const value=globalThis[key];if(!value)throw Error('Git streaming IO is not initialized');return value;}
export async function beginGitIo(files){
  const root=await navigator.storage.getDirectory();
  const directory=await root.getDirectoryHandle('git-work',{create:true});
  const name=crypto.randomUUID()+'.journal';
  const file=await directory.getFileHandle(name,{create:true});
  let handle;
  try{handle=await file.createSyncAccessHandle();}
  catch(error){await directory.removeEntry(name).catch(()=>{});throw error;}
  globalThis[key]={files,handle,file,directory,name,reader:new FileReaderSync(),length:0,stats:{bytesRead:0,maxRead:0,bytesStaged:0,maxAppend:0}};
}
export function gitRead(path,offset,length){
  const io=state();if(!Number.isSafeInteger(offset)||offset<0||length<0||length>CHUNK)throw Error('Invalid Git read range');
  let bytes;
  if(path===null||path===undefined){bytes=new Uint8Array(Math.min(length,Math.max(0,io.length-offset)));const n=io.handle.read(bytes,{at:offset});bytes=bytes.subarray(0,n);}
  else{const file=io.files[path];if(!(file instanceof Blob))throw Error('Git source file is missing: '+path);bytes=new Uint8Array(io.reader.readAsArrayBuffer(file.slice(offset,offset+length)));}
  io.stats.bytesRead+=bytes.length;io.stats.maxRead=Math.max(io.stats.maxRead,length);return bytes;
}
export function gitAppend(bytes){
  const io=state();if(bytes.length>CHUNK)throw Error('Git write exceeds the chunk budget');
  const start=io.length;let written=0;
  while(written<bytes.length){const n=io.handle.write(bytes.subarray(written),{at:io.length});if(!n)throw Error('Git staging storage is full');written+=n;io.length+=n;}
  io.stats.bytesStaged+=bytes.length;io.stats.maxAppend=Math.max(io.stats.maxAppend,bytes.length);return start;
}
export function gitJournalLength(){return state().length;}
export async function finishGitIo(){const io=state();io.handle.flush();io.handle.close();globalThis[key]=null;return {stage:await io.file.getFile(),temporary:io.name,stats:io.stats};}
export async function abortGitIo(){const io=globalThis[key];globalThis[key]=null;if(!io)return;try{io.handle.close();}catch{}await io.directory.removeEntry(io.name).catch(()=>{});}

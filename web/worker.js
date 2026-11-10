import {beginGitIo,finishGitIo,abortGitIo} from './git-io.js';
import init, { browser_stream_repository, browser_git_heap_bytes, browser_repository_paths, run_worker_job, parse_footprint_model_libraries, pcb_cache_version } from './pkg/vispace.js';
import { readKiCadFolder, readBoardModels, readProject3dCache, saveProject3dCache, pcbSourceStamps, readProjectPcbCache, saveProjectPcbCache } from './host.js';
const ready = init();
self.onmessage = async ({ data }) => {
  try {
    await ready;
    if (data?.localGitPaths) { self.postMessage({value:JSON.parse(browser_repository_paths(JSON.stringify(data.localGitPaths)))}); return; }
    if (data?.localGit || data?.gitStream) {
      const request=data.localGit??data.gitStream;
      const files=request.files;const metadata={};
      for(const [path,file]of Object.entries(files)){if(!(file instanceof Blob))throw Error('Git source must be a file handle snapshot');metadata[path]={size:file.size,modified:file.lastModified??0};}
      await beginGitIo(files);const before=browser_git_heap_bytes();
      try{
        const value=JSON.parse(browser_stream_repository(JSON.stringify({...request,files:metadata})));
        const disk=await finishGitIo();disk.stats.heapBefore=before;disk.stats.heapAfter=browser_git_heap_bytes();
        self.postMessage({value,...disk});
      }catch(error){await abortGitIo();throw error;}
      return;
    }
    const input=JSON.parse(data);
    if(input.Kicad?.folder)input.Kicad.files=await readKiCadFolder(input.Kicad.folder, 'schematics');
    if(input.KicadBoard?.folder) {
      const b=input.KicadBoard;
      b.stamps=await pcbSourceStamps(b.folder,b.path);
      b.cache=await readProjectPcbCache(b.folder,b.path,b.stamps,pcb_cache_version());
      if(!b.cache)b.files=await readKiCadFolder(b.folder,'board');
    }
    if(input.Board3d) {
      const b=input.Board3d;
      b.assets=await readBoardModels(b.board,b.path,b.folder,b.models_folder,text=>JSON.parse(parse_footprint_model_libraries(text)), b.demo);
      b.cache=await readProject3dCache(b.folder,b.path);
    }
    let result=JSON.parse(run_worker_job(JSON.stringify(input)));
    if(input.KicadBoard?.cache) {
      const b=input.KicadBoard;
      // An intact prefix does not guarantee an intact payload or stable source.
      const after=await pcbSourceStamps(b.folder,b.path);
      if(result.error||JSON.stringify(after)!==JSON.stringify(b.stamps)) {
        b.cache=null;b.stamps=after;b.files=await readKiCadFolder(b.folder,'board');
        result=JSON.parse(run_worker_job(JSON.stringify(input)));
      }
    }
    if(input.KicadBoard?.folder && result.value?.pcb_cache) {
      const b=input.KicadBoard;
      try {
        const after=await pcbSourceStamps(b.folder,b.path);
        if(JSON.stringify(after)===JSON.stringify(b.stamps))await saveProjectPcbCache(b.folder,b.path,result.value.pcb_cache);
      }catch{/* Cache failure never prevents viewing the board. */}
      delete result.value.pcb_cache;
    }
    const prepared=result.value?.prepared;
    if(input.Board3d&&prepared) {
      const b=input.Board3d;
      if(prepared!==b.cache) {
        self.postMessage(JSON.stringify({update:{message:'Saving .vspace.cache…'}}));
        try {
          const status=await saveProject3dCache(b.folder,b.path,prepared);
          self.postMessage(JSON.stringify({update:{cache_status:status}}));
        }catch(error){self.postMessage(JSON.stringify({update:{cache_status:`3D cache not saved: ${error}`}}));}
      }
    }
    self.postMessage(JSON.stringify(result));
  } catch (error) {
    const failure = {error: String(error)};
    self.postMessage((data?.gitBundle || data?.localGit || data?.localGitPaths || data?.gitStream) ? failure : JSON.stringify(failure));
  }
};

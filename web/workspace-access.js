// Background work checks permission; only the explicit reconnect control can request it.
export async function workspaceWritable(folder){
 if(!folder)return true;
 try{return typeof folder.queryPermission==='function'&&await folder.queryPermission({mode:'readwrite'})==='granted';}catch{return false;}
}
export async function requireWorkspacePermission(folder){
 if(!await workspaceWritable(folder))throw Error('Reconnect folder to restore write access. Changes are paused.');
}

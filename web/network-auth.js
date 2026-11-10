// Authentication framing only; all key handling and signature verification runs in Rust/WASM.
export class NetworkAuth {
 constructor(identity,save,changed,history){this.i=identity;this.save=save;this.changed=changed;this.pending=new Map();this.history=history;}
 challenge(){return{type:'auth_challenge',version:1,node_id:this.i.id(),public_key:this.i.public_key(),nonce:crypto.randomUUID()+crypto.randomUUID()};}
 start(p,send){p.authLocal=this.challenge();send(p.authLocal);p.status='Authenticating';p.authDeadline=setTimeout(()=>{if(!p.authenticated)p.socket.close(1008,'Pairing timeout');},600000);p.authDeadline.unref?.();}
 update(p,send){if(!p.authPeer||!p.authProven)return;const actions=this.i.ledger();if(!p.authenticated&&!p.authReady&&p.authLedger!==actions){send({type:'auth_membership',actions:JSON.parse(actions)});p.authLedger=actions;}const trusted=this.i.trusted(p.authPeer.node_id);if(!trusted&&p.authenticated)throw Error('Peer authorization revoked');if(trusted&&!p.authReady){p.authReady=true;send({type:'auth_ready'});}return trusted&&p.remoteAuthReady;}
 receive(p,m,send){
 if(m.type==='auth_challenge'){
  if(p.authPeer||m.version!==1||typeof m.nonce!=='string'||m.nonce.length<32||m.nonce.length>128||m.node_id===this.i.id())throw Error('Invalid identity challenge');
  p.authPeer=m;
  const payload={version:1,local:this.i.id(),remote:m.node_id,nonce:m.nonce,own_nonce:p.authLocal.nonce};
  send({type:'auth_proof',proof:JSON.parse(this.i.sign('vinventory.hello.v1',JSON.stringify(payload)))});
 }else if(m.type==='auth_proof'){
  if(!p.authPeer||p.authProven)throw Error('Unexpected identity proof');
  const a=p.authPeer,payload={version:1,local:a.node_id,remote:this.i.id(),nonce:p.authLocal.nonce,own_nonce:a.nonce};
  if(m.proof.public_key!==a.public_key||this.i.verify('vinventory.hello.v1',JSON.stringify(payload),JSON.stringify(m.proof))!==a.node_id)throw Error('Invalid identity proof');
  if(p.expectedId&&p.expectedId!==a.node_id)throw Error('Discovered node identity does not match the authenticated key');
  if(p.expectedHostname&&p.expectedHostname!==a.node_id+'.vispace.local')throw Error('Discovery hostname does not match the authenticated key');
  p.authProven = true;
  this.pending.set(a.node_id, [a.public_key, Date.now()]);
  p.status = 'Waiting for approval';
  if (p.approveRemote && this.i.trusted(this.i.id()) && !this.i.trusted(a.node_id) && !this.i.revoked(a.node_id)) {
   this.authorize(a.public_key, false);
  } else {
   this.changed();
  }
 }else if(m.type==='auth_membership'){
  if(!p.authProven)throw Error('Identity proof required');if(this.i.merge(JSON.stringify(m.actions)))this.save();
 }else if(m.type==='auth_ready'){if(!p.authProven)throw Error('Identity proof required');p.remoteAuthReady=true;}
 else throw Error('Expected authentication message');
 return this.update(p,send);
 }
 authorize(publicKey,revoked){this.i.approve(publicKey,revoked,JSON.stringify(this.history()));this.save();this.changed();}
 state() {
  const cutoff = Date.now() - 24 * 60 * 60 * 1000;
  for (const [id, [, seen]] of this.pending) {
   if (seen <= cutoff) this.pending.delete(id);
  }
  return {
   pending: JSON.parse(this.i.discover_peers(JSON.stringify(Object.fromEntries(this.pending)))),
   actions: JSON.parse(this.i.ledger()),
  };
 }
}

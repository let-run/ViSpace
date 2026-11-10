//! Conservative bundle retention. Repository objects and user files are never pruned.
use super::*;
use std::{collections::{BTreeMap,HashSet,VecDeque},sync::{Arc,Mutex,OnceLock},time::Duration};
fn gate(project:&str)->Arc<tokio::sync::RwLock<()>>{
    static GATES:OnceLock<Mutex<BTreeMap<String,Arc<tokio::sync::RwLock<()>>>>>=OnceLock::new();
    GATES.get_or_init(Default::default).lock().unwrap().entry(project.to_ascii_lowercase()).or_default().clone()
}
pub(super) fn activity(project:&str)->Result<tokio::sync::OwnedRwLockReadGuard<()>>{
    gate(project).try_read_owned().context("Bundle cache cleanup in progress; retry shortly")
}
pub(super) fn applied(bundle:&PendingBundle)->bool{
    bundle.viewer_applied && crate::ui::project_git::open(&bundle.managed_repository).ok().flatten()
        .and_then(|repo|checkout_tip(&repo).ok().flatten()).is_some_and(|tip|tip.commit==bundle.range.tip.to_hex())
}
pub(super) fn run(store:&crate::store::SharedStore)->Result<usize>{
    if crate::ui::project_git::load_preferences()?.preserve_bundles{return Ok(0)}
    let (projects,root)={let locked=store.lock().map_err(|_|anyhow::anyhow!("Inventory unavailable"))?;(locked.data.projects.clone(),locked.path.with_extension("git-bundles"))};
    let mut removed=0;
    for project in projects{
        if project.unresolved_git_tips().len()!=1 || sync_blocked(&project){continue;}
        let Some(tip)=project.unresolved_git_tips().first().and_then(|id|ObjectId::from_hex(id).ok()) else{continue};
        if project.kicad.as_ref().is_some_and(|link|link.revision.starts_with("git-snapshot:")&&link.revision!=format!("git-snapshot:{}:{}",project.id,tip.to_hex())){continue;}
        let Some(path)=project.kicad.as_ref().and_then(crate::ui::project_git::folder) else{continue};
        let Ok(Some(repo))=crate::ui::project_git::open(&path) else{continue};
        if !checkout_tip(&repo)?.is_some_and(|head|head.commit==tip.to_hex()){continue;}
        let Ok(id)=project.id.parse::<uuid::Uuid>() else{continue};
        let Ok(_exclusive)=gate(&project.id).try_write_owned() else{continue};
        removed+=prune(&repo,&root.join(id.to_string()),tip,Duration::from_secs(600))?;
    }
    Ok(removed)
}
fn prune(repo:&Repository,cache:&Path,tip:ObjectId,age:Duration)->Result<usize>{
    if !cache.is_dir(){return Ok(0)}
    // Walk commit ancestry once. No trees, blobs, or pack payloads are loaded.
    let mut reachable=HashSet::new();let mut queue=VecDeque::from([tip]);
    while let Some(id)=queue.pop_front(){if reachable.insert(id){queue.extend(repo.objects().read_commit(&id)?.parents);}}
    let mut removed=0;
    for entry in std::fs::read_dir(cache)?{
        let entry=entry?;let metadata=entry.metadata()?;if !metadata.is_file()||metadata.modified()?.elapsed().unwrap_or_default()<age{continue;}
        let name=entry.file_name();let Some(stem)=name.to_str().and_then(|n|n.strip_suffix(".bundle")) else{continue};
        let Some((base,end))=stem.split_once('-') else{continue};
        let Ok(end)=ObjectId::from_hex(end) else{continue};
        if !reachable.contains(&end){continue;}
        let base=if base=="full"{None}else{let Ok(id)=ObjectId::from_hex(base) else{continue};if !reachable.contains(&id){continue;}Some(id)};
        // Unknown/invalid files and unresolved branches are deliberately retained.
        if open_checked(&entry.path(),&BundleRange{base,tip:end}).is_err(){continue;}
        match std::fs::remove_file(entry.path()){Ok(())=>removed+=1,Err(error) if error.kind()==io::ErrorKind::NotFound=>{},Err(error)=>return Err(error.into())}
    }
    Ok(removed)
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]
    fn cache_activity_excludes_cleanup(){
        let read=activity("test-cleanup-gate").unwrap();assert!(gate("test-cleanup-gate").try_write_owned().is_err());drop(read);
        let write=gate("test-cleanup-gate").try_write_owned().unwrap();assert!(activity("test-cleanup-gate").is_err());drop(write);
    }
    #[test]
    fn imported_bundle_can_be_pruned_and_regenerated_with_identical_history()->Result<()>{
        use grit_lib::prelude::{InitOptions,Repository};
        let root=tempfile::tempdir()?;let source=root.path().join("source");let(sender,_)=Repository::init(&source,&InitOptions::default())?;
        sender.config().set_local("user.name","Test")?;sender.config().set_local("user.email","test@example.invalid")?;
        grit_lib::refs::write_symbolic_ref(sender.git_dir(),"HEAD","refs/heads/main")?;
        std::fs::write(source.join("board.kicad_pro"),"{}")?;
        let cancel=CancelToken::never();
        grit_lib::porcelain::commit::commit_with_scope(&sender,&grit_lib::porcelain::commit::CommitRequest::new("initial"),grit_lib::porcelain::commit::CommitScope::AutoStage,&mut grit_lib::progress::NullProgress,&cancel)?;
        let tip=sender.references().resolve(RefName::try_from("HEAD")?)?;
        let cache=root.path().join("cache");let range=BundleRange{base:None,tip};let bundle=prepare(&sender,&cache,&range,&cancel)?;
        let receiver=root.path().join("receiver");streaming::repository(&bundle,&tip.to_hex(),&receiver)?;
        let repo=crate::ui::project_git::open(&receiver)?.unwrap();
        std::fs::write(source.join("board.kicad_pro"),"not applied")?;
        grit_lib::porcelain::commit::commit_with_scope(&sender,&grit_lib::porcelain::commit::CommitRequest::new("not applied"),grit_lib::porcelain::commit::CommitScope::AutoStage,&mut grit_lib::progress::NullProgress,&cancel)?;
        let incoming=sender.references().resolve(RefName::try_from("HEAD")?)?;
        let pending=prepare(&sender,&cache,&BundleRange{base:None,tip:incoming},&cancel)?;
        assert_eq!(prune(&repo,&cache,tip,Duration::from_secs(600))?,0);
        assert_eq!(prune(&repo,&cache,tip,Duration::ZERO)?,1);assert!(!bundle.exists());assert!(pending.exists());
        let recreated=prepare(&repo,&cache,&range,&cancel)?;assert!(recreated.is_file());open_checked(&recreated,&range)?;
        let third=root.path().join("third");streaming::repository(&recreated,&tip.to_hex(),&third)?;assert_eq!(std::fs::read_to_string(third.join("board.kicad_pro"))?,"{}");
        Ok(())
    }
}

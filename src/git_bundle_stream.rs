//! Disk-backed bundle validation and relay assembly shared with browser Git.
use anyhow::{Context,Result};
use grit_lib::browser_repository::stream::{self, FileMeta, Io};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
    sync::{Arc, Mutex},
};
#[derive(Debug)]
struct DiskIo {
    files: BTreeMap<String, Mutex<File>>,
    journal: Mutex<File>,
}
impl Io for DiskIo {
    fn read_at(&self, key: Option<&str>, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        let mut file = match key {
            Some(k) => self
                .files
                .get(k)
                .ok_or_else(|| io::Error::other("Missing bundle"))?,
            None => &self.journal,
        }
        .lock()
        .map_err(|_| io::Error::other("Journal unavailable"))?;
        file.seek(SeekFrom::Start(offset))?;
        file.read(buffer)
    }
    fn append(&self, bytes: &[u8]) -> io::Result<u64> {
        let mut file = self
            .journal
            .lock()
            .map_err(|_| io::Error::other("Journal unavailable"))?;
        let offset = file.seek(SeekFrom::End(0))?;
        file.write_all(bytes)?;
        Ok(offset)
    }
    fn journal_len(&self) -> io::Result<u64> {
        Ok(self
            .journal
            .lock()
            .map_err(|_| io::Error::other("Journal unavailable"))?
            .metadata()?
            .len())
    }
}
pub(super) fn process(paths: &[&Path], tip: &str, destination: Option<&Path>) -> Result<()> {
    run(paths,tip,destination,None,false).map(|_|())
}
pub(super) fn repository(path:&Path,tip:&str,directory:&Path)->Result<Vec<String>> {
    if let Some(repo)=crate::ui::project_git::open(directory)? {
        if repo.objects().read_commit(&grit_lib::objects::ObjectId::from_hex(tip)?).is_ok() {
            advance(&repo,tip)?;
            return Ok(Vec::new());
        }
    }
    run(&[path],tip,None,Some(directory),true)
}
pub(super) fn receive(path:&Path,tip:&str,directory:&Path)->Result<()> {
    let repo=crate::ui::project_git::open(directory)?.context("Missing bundle base repository")?;
    let oid=grit_lib::objects::ObjectId::from_hex(tip)?;
    if repo.objects().read_commit(&oid).is_ok(){
        repo.references().update_direct(&grit_lib::refs::store::RefName::try_from(format!("refs/vinventory/bundles/{tip}").as_str())?,&oid)?;
        return Ok(())
    }
    run(&[path],tip,None,Some(directory),false).map(|_|())
}
fn advance(repo:&grit_lib::prelude::Repository,tip:&str)->Result<()> {
    use grit_lib::{refs::store::RefName,porcelain::status::{status,StatusOptions,UntrackedMode},progress::{NullProgress,CancelToken}};
    let tip=grit_lib::objects::ObjectId::from_hex(tip)?;
    let branch=grit_lib::refs::read_symbolic_ref_for_repository(repo,"HEAD")?.context("Managed repository has detached HEAD; automatic checkout paused")?;
    let reference=RefName::try_from(branch.as_str())?;
    let head=repo.references().resolve(&reference).ok();
    if head==Some(tip){return Ok(())}
    repo.references().update_direct(&RefName::try_from(format!("refs/vinventory/bundles/{}",tip.to_hex()).as_str())?,&tip)?;
    if let Some(head)=head {
        anyhow::ensure!(repo.revwalk().is_ancestor(&head.to_hex(),&tip.to_hex())?,"Managed repository has diverged; automatic checkout paused");
    }
    let options=StatusOptions{untracked:UntrackedMode::All,ahead_behind:false,write_index:false,..Default::default()};
    let state=status(repo,&options,&mut NullProgress,&CancelToken::never())?;
    anyhow::ensure!(state.is_clean() && state.untracked.is_empty() && state.conflicts.is_empty(),"Managed repository has local changes; automatic checkout paused");
    let old=head.map(|id|repo.objects().read_commit(&id).map(|commit|commit.tree)).transpose()?;
    let target=repo.objects().read_commit(&tip)?.tree;
    grit_lib::porcelain::checkout::checkout_between_trees(repo,old.as_ref(),&target)?;
    anyhow::ensure!(repo.references().resolve(&reference).ok()==head,"Branch changed during automatic checkout");
    repo.references().update_direct(&reference,&tip)?;
    Ok(())
}
fn run(paths: &[&Path], tip: &str, destination: Option<&Path>, checkout:Option<&Path>,update:bool) -> Result<Vec<String>> {
    let journal = tempfile::tempfile()?;
    let mut files = BTreeMap::new();
    let mut metadata = BTreeMap::new();
    let mut imports = Vec::new();
    for (index, path) in paths.iter().enumerate() {
        let key = format!("bundle-{index}");
        let file = File::open(path)?;
        metadata.insert(
            key.clone(),
            FileMeta {
                size: file.metadata()?.len(),
                modified: 0,
            },
        );
        files.insert(key.clone(), Mutex::new(file));
        imports.push(key);
    }
    // Make existing repository objects visible to prerequisite resolution and
    // emission. Known objects must never be recompressed for a new commit.
    if let Some(directory)=checkout {
        fn objects(root:&Path,dir:&Path,files:&mut BTreeMap<String,Mutex<File>>,metadata:&mut BTreeMap<String,FileMeta>)->Result<()> {
            if !dir.is_dir(){return Ok(())}
            for entry in std::fs::read_dir(dir)? {let path=entry?.path();
                if path.is_dir(){objects(root,&path,files,metadata)?;}else if path.is_file(){
                    let key=path.strip_prefix(root)?.to_string_lossy().replace('\\',"/");
                    let file=File::open(&path)?;metadata.insert(key.clone(),FileMeta{size:file.metadata()?.len(),modified:0});files.insert(key,Mutex::new(file));
                }
            }Ok(())
        }
        objects(directory,&directory.join(".git/objects"),&mut files,&mut metadata)?;
    }
    let io = Arc::new(DiskIo {
        files,
        journal: Mutex::new(journal),
    });
    let input = serde_json::from_value(
        serde_json::json!({"operation":if destination.is_some(){"bundle"}else if checkout.is_some(){"unpack"}else{"extract"},"files":metadata,"imports":imports,"tip":tip}),
    )?;
    let output =
        stream::execute(io.clone(), input).map_err(|error| anyhow::anyhow!(error.to_string()))?;
    if let Some(destination) = destination {
        let range = output
            .bundle
            .ok_or_else(|| io::Error::other("Missing relay bundle"))?;
        let mut staged = tempfile::NamedTempFile::new_in(destination.parent().unwrap())?;
        let mut journal = io.journal.lock().unwrap();
        journal.seek(SeekFrom::Start(range.offset))?;
        io::copy(
            &mut Read::by_ref(&mut *journal).take(range.length),
            &mut staged,
        )?;
        staged.as_file().sync_all()?;
        staged.persist(destination)?;
    }
    if let Some(directory)=checkout {
        let repo=if let Some(repo)=crate::ui::project_git::open(directory)?{repo}else{
            let (repo,_)=grit_lib::prelude::Repository::init(directory,&grit_lib::prelude::InitOptions::default())?;
            grit_lib::refs::write_symbolic_ref(repo.git_dir(),"HEAD","refs/heads/main")?;repo
        };
        for (path,range) in &output.writes {
            let relative=Path::new(path);
            anyhow::ensure!(relative.components().all(|c|matches!(c,std::path::Component::Normal(_))),"Unsafe snapshot path");
            let destination=directory.join(relative);
            std::fs::create_dir_all(destination.parent().unwrap())?;
            if destination.is_file(){continue}
            let mut target=tempfile::NamedTempFile::new_in(destination.parent().unwrap())?;
            let mut journal=io.journal.lock().unwrap();journal.seek(SeekFrom::Start(range.offset))?;
            io::copy(&mut Read::by_ref(&mut *journal).take(range.length),&mut target)?;
            match target.persist_noclobber(destination){Ok(_)=>{},Err(e) if e.error.kind()==io::ErrorKind::AlreadyExists=>{},Err(e)=>return Err(e.into())}
        }
        if update{advance(&repo,tip)?;}else{
            let oid=grit_lib::objects::ObjectId::from_hex(tip)?;
            repo.references().update_direct(&grit_lib::refs::store::RefName::try_from(format!("refs/vinventory/bundles/{tip}").as_str())?,&oid)?;
        }
    }
    Ok(output.choices)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stable_checkout_pauses_for_edits_untracked_files_and_divergence()->Result<()> {
        use grit_lib::{prelude::{Repository,InitOptions},porcelain::commit::{commit_with_scope,CommitRequest,CommitScope},progress::{NullProgress,CancelToken},refs::store::RefName};
        fn commit(repo:&Repository,text:&str)->Result<grit_lib::objects::ObjectId>{
            std::fs::write(repo.work_tree().unwrap().join("board.kicad_pro"),text)?;
            commit_with_scope(repo,&CommitRequest::new(text),CommitScope::AutoStage,&mut NullProgress,&CancelToken::never())?;
            Ok(repo.references().resolve(RefName::try_from("HEAD")?)?)
        }
        let root=tempfile::tempdir()?;
        let (sender,_)=Repository::init(&root.path().join("sender"),&InitOptions::default())?;
        sender.config().set_local("user.name","Test")?;sender.config().set_local("user.email","test@example.invalid")?;
        std::fs::write(sender.work_tree().unwrap().join("obsolete.txt"),"old")?;
        let first=commit(&sender,"first")?;
        let cache=root.path().join("cache");
        let bundle=crate::git_bundle::prepare(&sender,&cache,&crate::git_bundle::BundleRange{base:None,tip:first},&CancelToken::never())?;
        let directory=root.path().join("repository");std::fs::create_dir(&directory)?;
        super::repository(&bundle,&first.to_hex(),&directory)?;
        let receiver=crate::ui::project_git::open(&directory)?.unwrap();
        std::fs::remove_file(sender.work_tree().unwrap().join("obsolete.txt"))?;
        let second=commit(&sender,"second")?;
        let bundle=crate::git_bundle::prepare(&sender,&cache,&crate::git_bundle::BundleRange{base:None,tip:second},&CancelToken::never())?;
        std::fs::write(directory.join("board.kicad_pro"),"local edits")?;
        assert!(super::repository(&bundle,&second.to_hex(),&directory).unwrap_err().to_string().contains("local changes"));
        assert_eq!(std::fs::read_to_string(directory.join("board.kicad_pro"))?,"local edits");
        assert_eq!(receiver.references().resolve(RefName::try_from("HEAD")?)?,first);
        assert!(receiver.objects().read_commit(&second).is_ok());
        std::fs::write(directory.join("board.kicad_pro"),"first")?;
        std::fs::write(directory.join("untracked.txt"),"keep")?;
        assert!(super::repository(&bundle,&second.to_hex(),&directory).is_err());
        std::fs::remove_file(directory.join("untracked.txt"))?;
        super::repository(&bundle,&second.to_hex(),&directory)?;
        assert_eq!(std::fs::read_to_string(directory.join("board.kicad_pro"))?,"second");
        assert!(!directory.join("obsolete.txt").exists());
        receiver.config().set_local("user.name","Test")?;receiver.config().set_local("user.email","test@example.invalid")?;
        let local=commit(&receiver,"local commit")?;
        let third=commit(&sender,"third")?;
        let bundle=crate::git_bundle::prepare(&sender,&cache,&crate::git_bundle::BundleRange{base:None,tip:third},&CancelToken::never())?;
        assert!(super::repository(&bundle,&third.to_hex(),&directory).unwrap_err().to_string().contains("diverged"));
        assert_eq!(receiver.references().resolve(RefName::try_from("HEAD")?)?,local);
        assert_eq!(std::fs::read_to_string(directory.join("board.kicad_pro"))?,"local commit");
        assert!(receiver.objects().read_commit(&third).is_ok());
        Ok(())
    }
    #[test]
    fn indexed_packs_and_both_delta_encodings_are_read_on_demand() -> Result<()> {
        use grit_lib::{
            porcelain::commit::{CommitRequest, CommitScope, commit_with_scope},
            prelude::{InitOptions, Repository},
            progress::{CancelToken, NullProgress},
        };
        for ofs in [false, true] {
            let root = tempfile::tempdir()?;
            let (repo, _) = Repository::init(root.path(), &InitOptions::default())?;
            repo.config().set_local("user.name", "Stream Test")?;
            repo.config()
                .set_local("user.email", "stream@example.invalid")?;
            std::fs::write(root.path().join("board.kicad_pro"),"{}")?;
            for n in 0..6 {
                std::fs::write(
                    root.path().join("board.kicad_sch"),
                    format!("{}\n{n}", "(test data)\n".repeat(40000)),
                )?;
                commit_with_scope(
                    &repo,
                    &CommitRequest::new("fixture"),
                    CommitScope::AutoStage,
                    &mut NullProgress,
                    &CancelToken::never(),
                )?;
            }
            let mut options = grit_lib::maintenance::RepackOptions::default();
            let mut pack = grit_lib::objects::pack_objects::PackBuildOptions::for_local_copy(None);
            pack.use_ofs_delta = ofs;
            options.pack_build = Some(pack);
            options.threads = Some(1);
            let packed = grit_lib::maintenance::repack_all(&repo, &options)?;
            assert!(packed.pruned_loose > 0);
            fn scan(
                root: &Path,
                dir: &Path,
                files: &mut BTreeMap<String, Mutex<File>>,
                metadata: &mut BTreeMap<String, FileMeta>,
            ) -> Result<()> {
                for entry in std::fs::read_dir(dir)? {
                    let p = entry?.path();
                    if p.is_dir() {
                        scan(root, &p, files, metadata)?;
                    } else {
                        let name = p.strip_prefix(root)?.to_string_lossy().replace('\\', "/");
                        let file = File::open(&p)?;
                        metadata.insert(
                            name.clone(),
                            FileMeta {
                                size: file.metadata()?.len(),
                                modified: 0,
                            },
                        );
                        files.insert(name, Mutex::new(file));
                    }
                }
                Ok(())
            }
            let mut files = BTreeMap::new();
            let mut metadata = BTreeMap::new();
            scan(root.path(), root.path(), &mut files, &mut metadata)?;
            let io = Arc::new(DiskIo {
                files,
                journal: Mutex::new(tempfile::tempfile()?),
            });
            let input =
                serde_json::from_value(serde_json::json!({"operation":"status","files":metadata}))?;
            let output = stream::execute(io.clone(), input).map_err(|e| anyhow::anyhow!(e.to_string()))?;
            assert!(output.changes.is_empty());
            let input=serde_json::from_value(serde_json::json!({"operation":"share","files":metadata}))?;
            let shared=stream::execute(io,input).map_err(|e|anyhow::anyhow!(e.to_string()))?;
            assert!(shared.tip.is_some());assert_eq!(shared.tip,shared.head);
            assert!(shared.bundle.is_some());assert!(shared.writes.is_empty());
        }
        Ok(())
    }
}

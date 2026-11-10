//! Grit bundle creation and import for project history exchange.
//!
//! Transport callers must authorize the project and advertised tip before asking
//! for a bundle. An imported tip gets its own ref and never moves the checkout.
use anyhow::{Context, Result, ensure};
use grit_lib::{
    bundle::{Bundle, BundleSpec, write_bundle_with_cancel},
    objects::ObjectId,
    prelude::Repository,
    progress::CancelToken,
    refs::store::RefName,
};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
};

#[path = "git_bundle_stream.rs"]
mod streaming;
#[path = "git_bundle_cleanup.rs"]
mod cleanup;
const MAX_TRANSFER: u64 = 4 * 1024 * 1024 * 1024;
const TRANSFER_WINDOW: u64 = 1024 * 1024;

// Availability wakeups are advisory; polling remains the recovery path.
fn availability() -> &'static tokio::sync::broadcast::Sender<usize> {
    static CHANNEL: std::sync::OnceLock<tokio::sync::broadcast::Sender<usize>> = std::sync::OnceLock::new();
    CHANNEL.get_or_init(|| tokio::sync::broadcast::channel(128).0)
}
pub(crate) fn subscribe_ready() -> tokio::sync::broadcast::Receiver<usize> { availability().subscribe() }
fn announce_ready(store: &WeakStore) { let _ = availability().send(store.as_ptr() as usize); }

/// The base is absent for a full bundle. Both IDs are immutable commit IDs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleRange {
    pub base: Option<ObjectId>,
    pub tip: ObjectId,
}

impl BundleRange {
    /// A requested base can be used only if it is a strict ancestor of the tip.
    /// Unrelated or unavailable bases require a full bundle.
    pub fn negotiate(repo: &Repository, base: Option<ObjectId>, tip: ObjectId) -> Result<Self> {
        repo.odb()
            .read_commit(&tip)
            .context("Bundle tip is unavailable")?;
        let base = match base {
            Some(base)
                if base != tip
                    && repo.odb().read_commit(&base).is_ok()
                    && repo.revwalk().is_ancestor(&base.to_hex(), &tip.to_hex())? =>
            {
                Some(base)
            }
            _ => None,
        };
        Ok(Self { base, tip })
    }

    fn file_name(&self) -> String {
        let base = self.base.map_or_else(|| "full".into(), |oid| oid.to_hex());
        format!("{base}-{}.bundle", self.tip.to_hex())
    }

    fn ref_name(&self) -> String {
        format!("refs/vinventory/bundles/{}", self.tip.to_hex())
    }
}

/// Return an existing exact-range bundle, or generate it atomically using Grit.
/// The cache directory must be scoped to a single project by the caller.
pub fn prepare(
    repo: &Repository,
    cache: &Path,
    range: &BundleRange,
    cancel: &CancelToken,
) -> Result<PathBuf> {
    let path = cache.join(range.file_name());
    if path.is_file() {
        open_checked(&path, range)?;
        return Ok(path);
    }

    let _progress=crate::git_progress::Receive::creating();
    let negotiated = BundleRange::negotiate(repo, range.base, range.tip)?;
    ensure!(
        &negotiated == range,
        "Bundle base is not a strict ancestor of its tip"
    );
    std::fs::create_dir_all(cache)?;
    let mut file = tempfile::NamedTempFile::new_in(cache)?;
    let spec = BundleSpec {
        include: vec![(range.tip, range.ref_name())],
        exclude: range.base.into_iter().collect(),
        filter: None,
    };
    write_bundle_with_cancel(repo, &spec, &mut file, cancel)?;
    file.as_file().sync_all()?;
    match file.persist_noclobber(&path) {
        Ok(_) => {}
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            open_checked(&path, range)?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(path)
}

/// Import into a dedicated ref keyed by commit ID. Existing branch tips and
/// previously imported divergent tips remain intact.
pub fn import(
    repo: &Repository,
    path: &Path,
    range: &BundleRange,
    cancel: &CancelToken,
) -> Result<()> {
    let bundle = open_checked(path, range)?;
    if let Some(base) = range.base {
        repo.odb()
            .read_commit(&base)
            .context("Bundle base is unavailable")?;
    }
    bundle.unbundle_with_progress(repo, &mut grit_lib::progress::NullProgress, cancel)?;
    ensure!(
        BundleRange::negotiate(repo, range.base, range.tip)? == *range,
        "Bundle tip does not descend from the declared base"
    );
    let name = range.ref_name();
    repo.references()
        .update_direct(&RefName::try_from(name.as_str())?, &range.tip)?;
    Ok(())
}

/// Look up cached bytes by exact range without opening a Git checkout. This
/// also lets a relay forward an opaque file without needing to unpack it.
pub fn cached(cache: &Path, range: &BundleRange) -> Result<Option<File>> {
    match File::open(cache.join(range.file_name())) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Preserve a received bundle byte for byte. The caller must authenticate the
/// range and enforce its transfer quota before calling this method. A short or
/// oversized transfer never becomes visible in the cache.
pub fn preserve(
    cache: &Path,
    range: &BundleRange,
    mut input: impl Read,
    expected_bytes: u64,
) -> Result<PathBuf> {
    ensure!(expected_bytes > 0, "Bundle transfer is empty");
    std::fs::create_dir_all(cache)?;
    let mut file = tempfile::NamedTempFile::new_in(cache)?;
    let written = io::copy(&mut input.by_ref().take(expected_bytes), &mut file)?;
    ensure!(written == expected_bytes, "Bundle transfer ended early");
    ensure!(
        input.read(&mut [0])? == 0,
        "Bundle transfer exceeded its declared size"
    );
    file.as_file().sync_all()?;
    let path = cache.join(range.file_name());
    match file.persist_noclobber(&path) {
        Ok(_) => Ok(path),
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => Ok(path),
        Err(error) => Err(error.into()),
    }
}

fn open_checked(path: &Path, range: &BundleRange) -> Result<Bundle> {
    let bundle = Bundle::open(path)?;
    let header = bundle.header();
    ensure!(
        header.refs.len() == 1 && header.refs[0].1 == range.tip,
        "Bundle does not contain the requested tip"
    );
    if range.base.is_none() {
        ensure!(
            header.prerequisites.is_empty(),
            "A full bundle has prerequisites"
        );
    }
    Ok(bundle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use grit_lib::{
        porcelain::commit::{CommitRequest, CommitScope, commit_with_scope},
        prelude::InitOptions,
        progress::NullProgress,
    };

    fn commit(repo: &Repository, text: &str) -> Result<ObjectId> {
        std::fs::write(repo.work_tree().unwrap().join("board.kicad_pro"), text)?;
        commit_with_scope(
            repo,
            &CommitRequest::new(text),
            CommitScope::AutoStage,
            &mut NullProgress,
            &CancelToken::never(),
        )?;
        Ok(repo.references().resolve(RefName::try_from("HEAD")?)?)
    }

    fn init(path: &Path) -> Result<Repository> {
        let (repo, _) = Repository::init(path, &InitOptions::default())?;
        repo.config().set_local("user.name", "Bundle Test")?;
        repo.config()
            .set_local("user.email", "test@example.invalid")?;
        Ok(repo)
    }

    #[test]
    fn migrates_the_attached_version_to_a_stable_repository_preserving_edits()->Result<()> {
        use std::sync::{Arc,Mutex};
        let root=tempfile::tempdir()?;
        let sender=init(&root.path().join("sender"))?;
        let tip=commit(&sender,"initial")?;
        let store=Arc::new(Mutex::new(crate::store::Store::open(root.path().join("inventory.sqlite"))?));
        let project=store.lock().unwrap().change(|data|{data.ensure_project("Board")?;Ok(data.projects[0].id.clone())})?;
        let cache=root.path().join("inventory.git-bundles").join(project.parse::<uuid::Uuid>()?.to_string());
        let bundle=prepare(&sender,&cache,&BundleRange{base:None,tip},&CancelToken::never())?;
        let old=cache.join("repositories").join(tip.to_hex());std::fs::create_dir_all(&old)?;
        streaming::repository(&bundle,&tip.to_hex(),&old)?;
        std::fs::write(old.join("board.kicad_pro"),"local edits")?;
        let publisher=store.lock().unwrap().application_id().to_owned();
        store.lock().unwrap().change(|data|{
            let p=&mut data.projects[0];
            p.kicad=Some(crate::model::KicadProject{path:old.join("board.kicad_pro").to_string_lossy().into_owned(),revision:format!("git-snapshot:{project}"),..Default::default()});
            p.git_tips.insert(publisher.clone(),crate::model::GitTip{commit:tip.to_hex(),reference:"refs/heads/main".into(),includes:Vec::new()});Ok(())
        })?;
        assert!(pending_bundles(&Arc::downgrade(&store))?.iter().any(|bundle|bundle.project==project));
        apply_cached_snapshot(&Arc::downgrade(&store),&project,tip,&cache)?;
        let destination=cache.join("repository");
        assert!(!old.exists());assert!(destination.join(".git").is_dir());
        assert_eq!(std::fs::read_to_string(destination.join("board.kicad_pro"))?,"local edits");
        assert_eq!(store.lock().unwrap().data.projects[0].kicad.as_ref().unwrap().path,destination.join("board.kicad_pro").to_string_lossy());
        let link=store.lock().unwrap().data.projects[0].kicad.clone().unwrap();
        let target=root.path().join("relocated");std::fs::create_dir(&target)?;
        let relocated=relocate_managed(&link,&target)?;
        assert_eq!(std::fs::read_to_string(&relocated.path)?,"local edits");
        assert!(relocate_managed(&link,&target).is_ok());
        let invalid=root.path().join("invalid");std::fs::create_dir(&invalid)?;std::fs::write(invalid.join("keep.txt"),"keep")?;
        assert!(relocate_managed(&link,&invalid).is_err());assert_eq!(std::fs::read_to_string(invalid.join("keep.txt"))?,"keep");
        let wrong=init(&root.path().join("wrong"))?;commit(&wrong,"unrelated")?;
        assert!(relocate_managed(&link,wrong.work_tree().unwrap()).is_err());
        store.lock().unwrap().change(|data|{data.projects[0].kicad=Some(relocated.clone());Ok(())})?;
        assert_eq!(pending_bundles(&Arc::downgrade(&store))?[0].managed_repository,target.canonicalize()?);
        std::fs::write(&relocated.path,"initial")?;
        let next=commit(&sender,"next")?;prepare(&sender,&cache,&BundleRange{base:None,tip:next},&CancelToken::never())?;
        store.lock().unwrap().change(|data|{data.projects[0].git_tips.insert(publisher.clone(),crate::model::GitTip{commit:next.to_hex(),reference:"refs/heads/main".into(),includes:vec![tip.to_hex()]});Ok(())})?;
        apply_cached_snapshot(&Arc::downgrade(&store),&project,next,&cache)?;
        assert_eq!(std::fs::read_to_string(&relocated.path)?,"next");
        assert_eq!(store.lock().unwrap().data.projects[0].kicad.as_ref().unwrap().path,relocated.path);
        let report=diagnostics(&store,&project)?;
        assert_eq!(report["head"],next.to_hex());
        assert_eq!(report["history"][0]["hash"],next.to_hex());
        assert!(report["history"].as_array().unwrap().len()>=2);
        let page=diagnostics_limit(&store,&project,1)?;
        assert_eq!(page["history"].as_array().unwrap().len(),1);assert_eq!(page["history_more"],true);
        let complete=diagnostics_limit(&store,&project,30)?;assert_eq!(complete["history_more"],false);
        let item=store.lock().unwrap().data.projects[0].clone();assert!(!sync_blocked(&item));
        sync_errors().lock().unwrap().insert(project.clone(),"Local changes; automatic checkout paused".into());
        assert!(sync_blocked(&item));sync_errors().lock().unwrap().remove(&project);assert!(!sync_blocked(&item));
        let mut conflict=item;conflict.git_tips.insert("other".into(),crate::model::GitTip{commit:"f".repeat(40),reference:"refs/heads/main".into(),includes:Vec::new()});assert!(sync_blocked(&conflict));
        let receiver=crate::ui::project_git::open(&target.canonicalize()?)?.unwrap();
        receiver.config().set_local("user.name","Test")?;receiver.config().set_local("user.email","test@example.invalid")?;
        let local=commit(&receiver,"locally authored")?;
        publish_tips(&store)?;
        assert_eq!(store.lock().unwrap().data.projects[0].git_tips[&publisher].commit,local.to_hex());
        for n in 0..35{commit(&receiver,&format!("history page {n}"))?;}
        let first=diagnostics_limit(&store,&project,30)?;assert_eq!(first["history"].as_array().unwrap().len(),30);assert_eq!(first["history_more"],true);
        let expanded=diagnostics_limit(&store,&project,60)?;assert!(expanded["history"].as_array().unwrap().len()>30);assert_eq!(expanded["history_more"],false);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn checkoutless_desktop_relays_an_announced_bundle() -> Result<()> {
        use crate::{
            model::{GitRemote, KicadProject},
            store::Store,
            sync::{SyncClient, SyncServer},
        };
        use std::{
            sync::{Arc, Mutex},
            time::Duration,
        };

        let root = tempfile::tempdir()?;
        let checkout = root.path().join("checkout");
        let repo = init(&checkout)?;
        let base=commit(&repo,"initial")?;
        std::fs::create_dir_all(checkout.join("models"))?;
        std::fs::write(checkout.join("models/component.step"),"complete model data")?;
        std::fs::write(checkout.join(".project-settings"),"hidden tracked file")?;
        let tip = commit(&repo, "announced")?;
        let source = Arc::new(Mutex::new(Store::open(root.path().join("a.sqlite"))?));
        let middle = Arc::new(Mutex::new(Store::open(root.path().join("b.sqlite"))?));
        let end = Arc::new(Mutex::new(Store::open(root.path().join("c.sqlite"))?));
        let project = source.lock().unwrap().change(|data| {
            data.ensure_project("Board")?;
            let project = &mut data.projects[0];
            project.kicad = Some(KicadProject {
                path: checkout
                    .join("board.kicad_pro")
                    .to_string_lossy()
                    .into_owned(),
                ..Default::default()
            });
            project.git_remote = Some(GitRemote {
                url: "https://example.invalid/board.git".into(),
                ..Default::default()
            });
            Ok(project.id.clone())
        })?;
        let address = "127.0.0.1:0".parse().unwrap();
        for (x,y) in [(&source,&middle),(&middle,&source),(&middle,&end),(&end,&middle)]{let key=y.lock().unwrap().network_identity().public_key()?;x.lock().unwrap().network_authorize(key,false)?;}
        let host_a = SyncServer::start(source, address)?;
        let host_b = SyncServer::start(middle.clone(), address)?;
        let _ab = SyncClient::start(middle.clone(), &format!("wss://{}", host_a.address))?;
        let _bc = SyncClient::start(end.clone(), &format!("wss://{}", host_b.address))?;
        let range = BundleRange { base: None, tip };
        let id: uuid::Uuid = project.parse()?;
        let path = root
            .path()
            .join("c.git-bundles")
            .join(id.to_string())
            .join(range.file_name());
        tokio::time::timeout(Duration::from_secs(32), async {
            while !path.is_file() || middle.lock().unwrap().data.projects[0].kicad.is_none() || end.lock().unwrap().data.projects[0].kicad.is_none() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await?;
        assert_eq!(open_checked(&path, &range)?.header().refs[0].1, tip);
        for receiver in [&middle,&end] {
            let weak=std::sync::Arc::downgrade(receiver);
            let pending=pending_bundles(&weak)?;
            for bundle in pending {apply_cached_snapshot(&weak,&bundle.project,bundle.range.tip,&bundle.cache)?;}
            let locked=receiver.lock().unwrap();let link=locked.data.projects[0].kicad.as_ref().context("Missing automatic desktop snapshot")?;
            assert_eq!(std::fs::read_to_string(&link.path)?,"announced");
            let folder=Path::new(&link.path).parent().unwrap();
            assert_eq!(std::fs::read_to_string(folder.join("models/component.step"))?,"complete model data");
            assert_eq!(std::fs::read_to_string(folder.join(".project-settings"))?,"hidden tracked file");
            let materialized=crate::ui::project_git::open(folder)?.context("Missing materialized Git repository")?;
            assert_eq!(materialized.references().resolve(RefName::try_from("HEAD")?)?,tip);
            assert_eq!(materialized.objects().read_commit(&tip)?.parents,vec![base]);
            assert!(materialized.objects().read_commit(&base).is_ok());
            assert!(!materialized.index().load()?.entries().is_empty());
        }
        let stable=end.lock().unwrap().data.projects[0].kicad.as_ref().unwrap().path.clone();
        let next=commit(&repo,"incremental update")?;
        let delta=path.parent().unwrap().join(BundleRange{base:Some(tip),tip:next}.file_name());
        tokio::time::timeout(Duration::from_secs(40),async{
            loop {
                let ready={let locked=end.lock().unwrap();locked.data.projects[0].kicad.as_ref().is_some_and(|link|link.path==stable&&std::fs::read_to_string(&link.path).ok().as_deref()==Some("incremental update"))};
                if ready&&delta.is_file(){break}
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }).await?;
        assert!(std::fs::metadata(&delta)?.len()<64*1024);
        assert!(!path.parent().unwrap().join(BundleRange{base:None,tip:next}.file_name()).exists());
        Ok(())
    }

    #[test]
    fn forwards_an_incremental_bundle_without_moving_a_divergent_checkout() -> Result<()> {
        let root = tempfile::tempdir()?;
        let sender = init(&root.path().join("sender"))?;
        let receiver = init(&root.path().join("receiver"))?;
        let cache = root.path().join("cache");
        let cancel = CancelToken::never();
        let base = commit(&sender, "base")?;
        let initial = BundleRange {
            base: None,
            tip: base,
        };
        let path = prepare(&sender, &cache, &initial, &cancel)?;
        import(&receiver, &path, &initial, &cancel)?;

        let unrelated = commit(&receiver, "local work")?;
        let tip = commit(&sender, "incoming work")?;
        let range = BundleRange::negotiate(&sender, Some(base), tip)?;
        assert_eq!(range.base, Some(base));
        assert_eq!(
            BundleRange::negotiate(&sender, Some(unrelated), tip)?.base,
            None
        );
        let path = prepare(&sender, &cache, &range, &cancel)?;
        assert_eq!(prepare(&sender, &cache, &range, &cancel)?, path);
        assert!(cached(&cache, &range)?.is_some());

        let relay = root.path().join("relay");
        let bytes = std::fs::read(&path)?;
        let size = bytes.len() as u64;
        assert!(preserve(&relay, &range, &bytes[..], size + 1).is_err());
        assert!(preserve(&relay, &range, &bytes[..], size - 1).is_err());
        assert!(cached(&relay, &range)?.is_none());
        let forwarded = preserve(&relay, &range, &bytes[..], size)?;
        assert_eq!(std::fs::read(forwarded)?, bytes);

        let initial_path=cache.join(initial.file_name());
        streaming::process(&[initial_path.as_path()], &base.to_hex(),None)?;
        let rebuilt=cache.join("streamed-full.bundle");
        streaming::process(&[initial_path.as_path(),path.as_path()], &tip.to_hex(),Some(&rebuilt))?;
        streaming::process(&[rebuilt.as_path()], &tip.to_hex(),None)?;
        let corrupt=cache.join("corrupt.bundle");
        let mut damaged=std::fs::read(&rebuilt)?;let last=damaged.len()-1;damaged[last]^=1;std::fs::write(&corrupt,damaged)?;
        assert!(streaming::process(&[corrupt.as_path()], &tip.to_hex(),None).is_err());

        let empty = init(&root.path().join("empty"))?;
        assert!(import(&empty, &path, &range, &cancel).is_err());
        import(&receiver, &path, &range, &cancel)?;
        assert_eq!(
            receiver.references().resolve(RefName::try_from("HEAD")?)?,
            unrelated
        );
        let imported_ref = range.ref_name();
        assert_eq!(
            receiver
                .references()
                .resolve(RefName::try_from(imported_ref.as_str())?)?,
            tip
        );
        assert_eq!(
            std::fs::read_to_string(receiver.work_tree().unwrap().join("board.kicad_pro"))?,
            "local work"
        );
        Ok(())
    }
}

// A grant is valid only while the issuing sync session is connected.
type WeakStore = std::sync::Weak<std::sync::Mutex<crate::store::Store>>;
static GRANTS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::BTreeMap<uuid::Uuid, (WeakStore,String)>>,
> = std::sync::LazyLock::new(Default::default);
static TRANSFERS: std::sync::LazyLock<tokio::sync::Semaphore> =
    std::sync::LazyLock::new(|| tokio::sync::Semaphore::new(4));

static DOWNLOADS: std::sync::LazyLock<tokio::sync::Semaphore> =
    std::sync::LazyLock::new(|| tokio::sync::Semaphore::new(2));

static HOSTS: std::sync::LazyLock<std::sync::Mutex<std::collections::BTreeMap<usize, (u16,Option<String>)>>> =
    std::sync::LazyLock::new(Default::default);

pub(crate) struct Host(usize);
impl Host {
    pub(crate) fn register(store: &crate::store::SharedStore, port: u16, hostname:Option<String>) -> Self {
        let key = std::sync::Arc::as_ptr(store) as usize;
        HOSTS.lock().unwrap().insert(key, (port,hostname));
        Self(key)
    }

    pub(crate) fn port(store: &WeakStore) -> Option<u16> {
        HOSTS
            .lock()
            .unwrap()
            .get(&(store.as_ptr() as usize))
            .map(|entry|entry.0)
    }
    pub(crate) fn hostname(store:&WeakStore)->Option<String>{HOSTS.lock().unwrap().get(&(store.as_ptr() as usize)).and_then(|entry|entry.1.clone())}
}
impl Drop for Host {
    fn drop(&mut self) {
        HOSTS.lock().unwrap().remove(&self.0);
    }
}

fn require_grant(store:&WeakStore,grant:uuid::Uuid)->Result<()> {
    let (issuer,peer)=GRANTS.lock().unwrap().get(&grant).cloned().context("Sync authorization expired")?;
    ensure!(issuer.ptr_eq(store)&&store.upgrade().is_some_and(|s|s.lock().unwrap().network_trusted(&peer)),"Sync authorization expired or revoked");Ok(())
}
pub(crate) struct Grant(pub uuid::Uuid);
impl Grant {
    pub(crate) fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
    pub(crate) fn activate(&self, store: WeakStore, peer:String) {
        GRANTS.lock().unwrap().insert(self.0, (store,peer));
    }
}
impl Drop for Grant {
    fn drop(&mut self) {
        GRANTS.lock().unwrap().remove(&self.0);
    }
}

#[derive(serde::Deserialize)]
struct Request {
    grant: uuid::Uuid,
    project: String,
    base: Option<String>,
    tip: Option<String>,
    #[serde(default)]
    upload: Option<Upload>,
    #[serde(default)]
    flow: bool,
}

#[derive(serde::Deserialize)]
struct Upload { size: u64, sha256: String }

struct Transfer {
    range: BundleRange,
    file: PathBuf,
    _temp: Option<tempfile::TempDir>,
}

fn prepare_transfer(store: WeakStore, request: Request) -> Result<Option<Transfer>> {
    let requested = request.tip.as_deref().map(ObjectId::from_hex).transpose()?;
    let base = request
        .base
        .as_deref()
        .map(ObjectId::from_hex)
        .transpose()?;
    let store = store.upgrade().context("Inventory closed")?;
    let (path, cache) = {
        let store = store
            .lock()
            .map_err(|_| anyhow::anyhow!("Inventory unavailable"))?;
        let project = store
            .data
            .projects
            .iter()
            .find(|project| project.id == request.project)
            .context("Project is unavailable")?;
        if let Some(tip) = requested {
            ensure!(
                project
                    .git_tips
                    .values()
                    .any(|value| value.commit == tip.to_hex()),
                "Requested commit has not been announced for this project"
            );
        }
        let folder = project
            .kicad
            .as_ref()
            .and_then(crate::ui::project_git::folder);
        let id: uuid::Uuid = project.id.parse().context("Invalid project ID")?;
        (
            folder,
            store
                .path
                .with_extension("git-bundles")
                .join(id.to_string()),
        )
    };
    if let Some(tip) = requested {
        if base == Some(tip) {
            return Ok(None);
        }
        for base in [base].into_iter().chain((path.is_none() && base.is_some()).then_some(None)) {
            let range = BundleRange { base, tip };
            let file = cache.join(range.file_name());
            if file.is_file() {
                return Ok(Some(Transfer {
                    range,
                    file,
                    _temp: None,
                }));
            }
        }
    }
    let path = path.context("Peer has no checkout or cached bundle for this project")?;
    let repo = crate::ui::project_git::open(&path)?
        .context("Peer has no Git checkout for this project")?;
    let head = checkout_tip(&repo)?.context("Peer has no checked-out branch")?;
    let head = ObjectId::from_hex(&head.commit)?;
    let tip = requested.unwrap_or(head);
    repo.objects().read_commit(&tip).context("Requested commit is unavailable")?;
    if base == Some(tip) {
        return Ok(None);
    }
    let range = BundleRange::negotiate(&repo, base, tip)?;
    let keep = crate::ui::project_git::load_preferences()?.preserve_bundles;
    let temp = if keep {
        None
    } else {
        Some(tempfile::tempdir()?)
    };
    let destination = temp.as_ref().map_or(cache.as_path(), |temp| temp.path());
    let file = prepare(&repo, destination, &range, &CancelToken::new())?;
    Ok(Some(Transfer {
        range,
        file,
        _temp: temp,
    }))
}

fn sha256(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hex_digest(hasher))
}

fn hex_digest(hasher: Sha256) -> String {
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) async fn serve<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send>(
    mut ws: tokio_tungstenite::WebSocketStream<S>,
    store: WeakStore,
) -> Result<()> {
    use futures_util::{SinkExt, StreamExt};
    use std::time::Duration;
    use tokio::io::AsyncReadExt;
    use tokio_tungstenite::tungstenite::Message;
    let timeout = Duration::from_secs(30);
    let result = async {
        let text = tokio::time::timeout(timeout, ws.next())
            .await?
            .context("Missing bundle request")??;
        let Message::Text(text) = text else {
            anyhow::bail!("Expected a bundle request")
        };
        ensure!(text.len() <= 2048, "Bundle request is too large");
        let request: Request = serde_json::from_str(&text)?;
        require_grant(&store,request.grant)?;
        let _cache_activity=cleanup::activity(&request.project)?;
        let permit = TRANSFERS
            .try_acquire()
            .context("Peer is busy with bundle transfers")?;
        if request.upload.is_some() {
            return receive_upload(&mut ws, store, request).await;
        }
        let flow = request.flow;
        let grant=request.grant;let authorized_store=store.clone();
        let transfer = crate::async_io::blocking(move || prepare_transfer(store, request)).await?;
        let Some(transfer) = transfer else {
            tokio::time::timeout(
                timeout,
                ws.send(Message::Text(r#"{"current":true}"#.into())),
            )
            .await??;
            return Ok(());
        };
        let mut file = tokio::fs::File::open(&transfer.file).await?;
        let size = file.metadata().await?.len();
        ensure!(
            size <= MAX_TRANSFER,
            "Bundle exceeds the 4 GiB transfer limit"
        );
        let digest_path = transfer.file.clone();
        let sha256 = crate::async_io::blocking(move || sha256(&digest_path)).await?;
        let header = serde_json::json!({
            "base":transfer.range.base.map(|id| id.to_hex()), "tip":transfer.range.tip.to_hex(),
            "size":size, "sha256":sha256,
        });
        tokio::time::timeout(timeout, ws.send(Message::Text(header.to_string().into()))).await??;
        let mut buffer = vec![0; 64 * 1024];
        let mut sent=0u64; let mut acknowledged=0u64;
        loop {
            require_grant(&authorized_store,grant)?;
            let count = file.read(&mut buffer).await?;
            if count == 0 {
                break;
            }
            tokio::time::timeout(
                timeout,
                ws.send(Message::Binary(buffer[..count].to_vec().into())),
            )
            .await??;
            sent+=count as u64;
            if flow && (sent-acknowledged>=TRANSFER_WINDOW || sent==size) {
                let reply=tokio::time::timeout(timeout,ws.next()).await?.context("Missing transfer acknowledgement")??;
                let Message::Text(reply)=reply else {anyhow::bail!("Invalid transfer acknowledgement")};
                let reply:serde_json::Value=serde_json::from_str(&reply)?;
                ensure!(reply["received"].as_u64()==Some(sent),"Invalid transfer credit"); acknowledged=sent;
            }
        }
        drop(permit);
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if let Err(error) = result {
        let message = serde_json::json!({"error":error.to_string()});
        let _ =
            tokio::time::timeout(timeout, ws.send(Message::Text(message.to_string().into()))).await;
    }
    let _ = tokio::time::timeout(timeout, ws.close(None)).await;
    Ok(())
}

fn checkout_tip(repo: &Repository) -> Result<Option<crate::model::GitTip>> {
    let Some(reference) = grit_lib::refs::read_symbolic_ref_for_repository(repo, "HEAD")?
        .filter(|name| name.starts_with("refs/heads/"))
    else {
        return Ok(None);
    };
    let name = RefName::try_from(reference.as_str())?;
    let Some(_) = repo.ref_store().read_raw(name.as_ref())? else {
        return Ok(None);
    };
    let commit = repo.references().resolve(&name)?.to_hex();
    Ok(Some(crate::model::GitTip {
        reference,
        commit,
        includes: Vec::new(),
    }))
}

fn publish_tips(store: &crate::store::SharedStore) -> Result<()> {
    let (publisher, projects) = {
        let store = store
            .lock()
            .map_err(|_| anyhow::anyhow!("Inventory unavailable"))?;
        let publisher = store.application_id().to_owned();
        let projects = store
            .data
            .projects
            .iter()
            .filter_map(|project| {
                let remote = project.git_remote.clone();
                let link = project.kicad.as_ref()?;
                let path = crate::ui::project_git::folder(link)?;
                Some((
                    project.id.clone(),
                    link.path.clone(),
                    link.revision.starts_with("git-snapshot:"),
                    path,
                    remote.clone(),
                    project.git_tips.get(&publisher).cloned(),
                    project
                        .git_tips
                        .values()
                        .map(|tip| tip.commit.clone())
                        .collect::<std::collections::BTreeSet<_>>(),
                ))
            })
            .collect::<Vec<_>>();
        (publisher, projects)
    };
    let mut changes = Vec::new();
    for (id, link, managed, path, remote, previous, candidates) in projects {
        let Ok(Some(repo)) = crate::ui::project_git::open(&path) else {
            continue;
        };
        let Ok(Some(mut tip)) = checkout_tip(&repo) else {
            continue;
        };
        if managed && candidates.contains(&tip.commit){continue;}
        for candidate in candidates {
            if candidate != tip.commit
                && ObjectId::from_hex(&candidate)
                    .ok()
                    .is_some_and(|oid| repo.odb().read_commit(&oid).is_ok())
                && repo
                    .revwalk()
                    .is_ancestor(&candidate, &tip.commit)
                    .unwrap_or(false)
            {
                tip.includes.push(candidate);
            }
        }
        if previous.as_ref() != Some(&tip) {
            changes.push((id, link, remote, tip));
        }
    }
    if changes.is_empty() {
        return Ok(());
    }
    let weak = std::sync::Arc::downgrade(store);
    let mut store = store
        .lock()
        .map_err(|_| anyhow::anyhow!("Inventory unavailable"))?;
    store.change(|data| {
        for (id, link, remote, tip) in changes {
            if let Some(project) = data.projects.iter_mut().find(|p| {
                p.id == id
                    && p.kicad.as_ref().is_some_and(|k| k.path == link)
                    && p.git_remote == remote
            }) {
                project.git_tips.insert(publisher.clone(), tip);
            }
        }
        Ok(())
    })?;
    announce_ready(&weak);
    Ok(())
}

pub(crate) async fn watch(store: crate::store::SharedStore) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut cleanup_tick=0u32;
    loop {
        tick.tick().await;
        let clean=cleanup_tick.is_multiple_of(12);cleanup_tick=cleanup_tick.wrapping_add(1);
        let store = store.clone();
        let _ = crate::async_io::blocking(move || {
            let weak=std::sync::Arc::downgrade(&store);
            for bundle in pending_bundles(&weak)? {
                if cached_tip(&bundle.cache,bundle.range.tip).is_some() {
                    if let Err(error)=apply_cached_snapshot(&weak,&bundle.project,bundle.range.tip,&bundle.cache){eprintln!("Git snapshot pending: {error:#}");}
                }
            }
            publish_tips(&store)?;
            if clean{if let Err(error)=cleanup::run(&store){eprintln!("Git cache cleanup deferred: {error:#}");}}
            Ok::<_,anyhow::Error>(())
        }).await;
    }
}

#[derive(serde::Deserialize)]
struct TransferHeader {
    base: Option<String>,
    tip: String,
    size: u64,
    sha256: String,
}

fn cached_tip(cache:&Path,tip:ObjectId)->Option<PathBuf>{
    let full=cache.join(BundleRange{base:None,tip}.file_name());if full.is_file(){return Some(full)}
    let suffix=format!("-{}.bundle",tip.to_hex());
    std::fs::read_dir(cache).ok()?.filter_map(|entry|entry.ok()).map(|e|e.path()).find(|p|p.is_file()&&p.file_name().is_some_and(|n|n.to_string_lossy().ends_with(&suffix)))
}

struct PendingBundle {
    project: String,
    range: BundleRange,
    folder: Option<PathBuf>,
    managed_repository:PathBuf,
    viewer_applied:bool,
    cache: PathBuf,
}

fn pending_bundles(store: &WeakStore) -> Result<Vec<PendingBundle>> {
    let store = store.upgrade().context("Inventory closed")?;
    let store = store
        .lock()
        .map_err(|_| anyhow::anyhow!("Inventory unavailable"))?;
    let mut pending = Vec::new();
    for project in &store.data.projects {
        let Ok(id) = project.id.parse::<uuid::Uuid>() else {
            continue;
        };
        let cache = store
            .path
            .with_extension("git-bundles")
            .join(id.to_string());
        let folder = project
            .kicad
            .as_ref()
            .filter(|link|!link.revision.starts_with("git-snapshot:"))
            .and_then(crate::ui::project_git::folder);
        let local_tip = project
            .git_tips
            .get(store.application_id())
            .map(|tip| tip.commit.as_str());
        for tip in project.unresolved_git_tips() {
            if folder.is_some() && local_tip == Some(tip) {
                continue;
            }
            if let Ok(tip) = ObjectId::from_hex(tip) {
                pending.push(PendingBundle {
                    project: project.id.clone(),
                    range: BundleRange { base: None, tip },
                    folder: folder.clone(),
                    managed_repository:project.kicad.as_ref().and_then(|link|managed_root(link).ok()).unwrap_or_else(||cache.join("repository")),
                    viewer_applied:project.kicad.as_ref().is_some_and(|link|!link.revision.starts_with("git-snapshot:")||link.revision==format!("git-snapshot:{}:{}",project.id,tip.to_hex())),
                    cache: cache.clone(),
                });
            }
        }
    }
    Ok(pending)
}

async fn receive(url: &url::Url, grant: uuid::Uuid, bundle: PendingBundle, trust: crate::network_tls::PeerTls) -> Result<()> {
    let _cache_activity=cleanup::activity(&bundle.project)?;
    use futures_util::{SinkExt, StreamExt};
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;
    use tokio_tungstenite::tungstenite::Message;
    let timeout = Duration::from_secs(60);
    let permit = DOWNLOADS
        .try_acquire()
        .context("Bundle transfers are busy")?;
    let (mut ws, _) = tokio::time::timeout(
        timeout,
        crate::network_tls::connect_peer(
            url.as_str(),
            Some(
                tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
                    .max_message_size(Some(64 * 1024))
                    .max_frame_size(Some(64 * 1024)),
            ),
            false,
            trust,
        ),
    )
    .await??;
    let repository=bundle.managed_repository.clone();
    let base=crate::async_io::blocking(move||{
        Ok(crate::ui::project_git::open(&repository)?.and_then(|repo|checkout_tip(&repo).ok().flatten()).map(|tip|tip.commit))
    }).await?;
    let request = serde_json::json!({
        "grant":grant, "project":bundle.project, "tip":bundle.range.tip.to_hex(),"base":base,
    });
    tokio::time::timeout(timeout, ws.send(Message::Text(request.to_string().into()))).await??;
    let response = tokio::time::timeout(timeout, ws.next())
        .await?
        .context("Missing bundle header")??;
    let Message::Text(response) = response else {
        anyhow::bail!("Missing bundle header")
    };
    let header: TransferHeader = serde_json::from_str(&response)?;
    ensure!(
        (header.base.is_none() || header.base==base)
            && ObjectId::from_hex(&header.tip)? == bundle.range.tip
            && (1..=MAX_TRANSFER).contains(&header.size),
        "Unexpected bundle transfer range or size"
    );

    tokio::fs::create_dir_all(&bundle.cache).await?;
    let temp = tempfile::NamedTempFile::new_in(&bundle.cache)?;
    let mut file = tokio::fs::File::from_std(temp.reopen()?);
    let mut progress=crate::git_progress::Receive::new(header.size);
    let mut size = 0;
    let mut hasher = Sha256::new();
    while size < header.size {
        let data = tokio::time::timeout(timeout, ws.next())
            .await?
            .context("Bundle ended early")??;
        let Message::Binary(data) = data else {
            anyhow::bail!("Expected bundle bytes")
        };
        size += data.len() as u64;
        ensure!(
            !data.is_empty() && size <= header.size,
            "Invalid bundle frame length"
        );
        hasher.update(&data);
        file.write_all(&data).await?;
        progress.set(size,header.size);
    }
    progress.applying();
    ensure!(
        hex_digest(hasher) == header.sha256,
        "Bundle checksum mismatch"
    );
    file.sync_all().await?;
    drop(file);
    crate::async_io::blocking(move || {
        let actual=BundleRange{base:header.base.as_deref().map(ObjectId::from_hex).transpose()?,tip:bundle.range.tip};
        let path = bundle.cache.join(actual.file_name());
        open_checked(temp.path(), &actual)?.pack_object_count()?;
        let managed=bundle.managed_repository.clone();
        if managed.join(".git").is_dir(){streaming::receive(temp.path(),&actual.tip.to_hex(),&managed)?;}
        else {streaming::process(&[temp.path()],&actual.tip.to_hex(),None)?;}
        if let Some(folder) = &bundle.folder
            && let Ok(Some(repo)) = crate::ui::project_git::open(folder)
        {
            // Retain the bytes for forwarding even if the local repository cannot import them.
            let _ = import(&repo, temp.path(), &bundle.range, &CancelToken::new());
        }
        match temp.persist_noclobber(&path) {
            Ok(_) => Ok(()),
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(error.into()),
        }
    })
    .await?;
    drop(permit);
    Ok(())
}

fn managed_root(link:&crate::model::KicadProject)->Result<PathBuf>{
    let parent=Path::new(&link.path).parent().context("Invalid KiCad path")?;
    let repo=Repository::discover(Some(parent),&grit_lib::prelude::RepositoryOptions::empty())?;
    Ok(repo.work_tree().context("Repository has no working directory")?.to_owned())
}
pub(crate) fn relocate_managed(link:&crate::model::KicadProject,target:&Path)->Result<crate::model::KicadProject>{
    ensure!(link.revision.starts_with("git-snapshot:"),"This project is not automatically managed");
    let source=managed_root(link)?;
    ensure!(source.join(".git").is_dir(),"Relocation requires a standalone Git repository");
    let source_repo=crate::ui::project_git::open(&source)?.context("Source Git repository is missing")?;
    let head=checkout_tip(&source_repo)?.context("Source repository has no commit")?.commit;
    let target=target.canonicalize()?;let source=source.canonicalize()?;
    ensure!(!target.starts_with(&source)||target==source,"Choose a folder outside the current repository");
    if target!=source {
        if std::fs::read_dir(&target)?.next().is_none(){
            let stage=tempfile::tempdir_in(target.parent().context("Invalid destination")?)?;
            fn copy(source:&Path,target:&Path)->Result<()>{
                for entry in std::fs::read_dir(source)? {let entry=entry?;let path=target.join(entry.file_name());let kind=entry.file_type()?;
                    if kind.is_symlink(){
                        let destination=std::fs::read_link(entry.path())?;
                        #[cfg(unix)] std::os::unix::fs::symlink(destination,&path)?;
                        #[cfg(windows)] {if entry.path().is_dir(){std::os::windows::fs::symlink_dir(destination,&path)?;}else{std::os::windows::fs::symlink_file(destination,&path)?;}}
                    }else if kind.is_dir(){std::fs::create_dir(&path)?;copy(&entry.path(),&path)?;}else if kind.is_file(){std::fs::copy(entry.path(),&path)?;}
                }Ok(())
            }
            copy(&source,stage.path())?;
            ensure!(checkout_tip(&source_repo)?.is_some_and(|tip|tip.commit==head),"Source changed while copying; retry");
            std::fs::remove_dir(&target)?;std::fs::rename(stage.path(),&target)?;
        }else{
            ensure!(target.join(".git").is_dir(),"Choose an empty folder or a standalone Git repository at the same commit");
            let repo=crate::ui::project_git::open(&target)?.context("Choose an empty folder or a Git repository at the same commit")?;
            ensure!(checkout_tip(&repo)?.is_some_and(|tip|tip.commit==head),"Destination repository HEAD differs from the attached repository");
        }
    }
    let canonical_project=std::fs::canonicalize(&link.path)?;
    let relative=canonical_project.strip_prefix(&source)?;
    ensure!(target.join(relative).is_file(),"The matching repository does not contain the attached KiCad project file");
    let mut link=link.clone();link.path=target.join(relative).to_string_lossy().into_owned();Ok(link)
}

// Managed snapshots are local projections. Never overwrite a user-owned checkout,
// and recheck ancestry after extraction so a delayed transfer cannot win a race.
fn sync_errors()-> &'static std::sync::Mutex<std::collections::BTreeMap<String,String>>{
    static ERRORS:std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeMap<String,String>>>=std::sync::OnceLock::new();ERRORS.get_or_init(Default::default)
}
fn apply_cached_snapshot(store:&WeakStore,project_id:&str,tip:ObjectId,cache:&Path)->Result<()> {
    let _cache_activity=cleanup::activity(project_id)?;
    let _progress=crate::git_progress::Receive::processing();
    let result=apply_cached_snapshot_inner(store,project_id,tip,cache);
    if let Ok(mut errors)=sync_errors().lock(){match &result{Ok(())=>{errors.remove(project_id);},Err(error)=>{errors.insert(project_id.into(),format!("{error:#}"));}}}
    result
}
/// Read-only diagnostics; repository objects stay on the background worker.
pub(crate) fn diagnostics(store:&crate::store::SharedStore,id:&str)->Result<serde_json::Value>{diagnostics_limit(store,id,100)}
pub(crate) fn sync_blocked(project:&crate::model::Project)->bool{
    project.unresolved_git_tips().len()>1||sync_errors().lock().ok().is_some_and(|errors|errors.contains_key(&project.id))
}
pub(crate) fn diagnostics_limit(store:&crate::store::SharedStore,id:&str,limit:usize)->Result<serde_json::Value>{
    let (project,cache)={let store=store.lock().map_err(|_|anyhow::anyhow!("Inventory unavailable"))?;let project=store.data.projects.iter().find(|p|p.id==id).cloned().context("Project no longer exists")?;let cache=store.path.with_extension("git-bundles").join(id.parse::<uuid::Uuid>()?.to_string());(project,cache)};
    let mut report=serde_json::json!({"head":null,"history":[],"bundles":[],"events":[]});
    if let Some(error)=sync_errors().lock().ok().and_then(|errors|errors.get(id).cloned()){report["error"]=error.into();}
    if let Some(path)=project.kicad.as_ref().and_then(crate::ui::project_git::folder){
        match crate::ui::project_git::open(&path){
            Ok(Some(repo))=>{if let Some(tip)=checkout_tip(&repo)?{
                report["head"]=tip.commit.clone().into();
                let mut queue=std::collections::VecDeque::from([ObjectId::from_hex(&tip.commit)?]);let mut seen=std::collections::HashSet::new();let mut history=Vec::new();
                while let Some(hash)=queue.pop_front(){if !seen.insert(hash){continue;}let commit=repo.odb().read_commit(&hash)?;history.push(serde_json::json!({"hash":hash.to_hex(),"author":commit.author,"message":commit.message.lines().next().unwrap_or("").chars().take(500).collect::<String>()}));if history.len()>limit{break;}queue.extend(commit.parents);}
                report["history_more"]=(history.len()>limit).into();history.truncate(limit);report["history"]=history.into();
            }},Ok(None)=>{},Err(error)=>report["error"]=format!("{error:#}").into()
        }
    }
    let mut bundles=Vec::new();if let Ok(entries)=std::fs::read_dir(cache){for entry in entries.flatten(){if entry.path().extension().is_some_and(|ext|ext=="bundle"){bundles.push(serde_json::json!({"tips":[entry.file_name().to_string_lossy()],"bases":[]}));}}}report["bundles"]=bundles.into();Ok(report)
}

fn apply_cached_snapshot_inner(store:&WeakStore,project_id:&str,tip:ObjectId,cache:&Path)->Result<()> {
    let shared=store.upgrade().context("Inventory closed")?;
    // Import competing history into the actual linked repository, regardless of
    // whether its branch can advance. Manual resolution needs these refs.
    let linked={let locked=shared.lock().map_err(|_|anyhow::anyhow!("Inventory unavailable"))?;locked.data.projects.iter().find(|p|p.id==project_id).and_then(|p|p.kicad.as_ref()).and_then(|link|managed_root(link).ok())};
    if let (Some(root),Some(source))=(linked.as_ref(),cached_tip(cache,tip)){
        if let Ok(Some(repo))=crate::ui::project_git::open(root){
            let reference=format!("refs/vinventory/bundles/{}",tip.to_hex());
            let known=RefName::try_from(reference.as_str()).ok().and_then(|name|repo.references().resolve(&name).ok())==Some(tip);
            if !known{streaming::receive(&source,&tip.to_hex(),root)?;}
        }
    }
    let eligible=|project:&crate::model::Project| {
        let tips=project.unresolved_git_tips();
        tips.len()==1 && tips.contains(tip.to_hex().as_str())
    };
    std::fs::create_dir_all(cache)?;
    let gate=std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(cache.join("repository.lock"))?;
    if gate.try_lock().is_err(){return Ok(())}
    let previous={
        let locked=shared.lock().map_err(|_|anyhow::anyhow!("Inventory unavailable"))?;
        let Some(project)=locked.data.projects.iter().find(|p|p.id==project_id).filter(|p|eligible(p)) else {return Ok(())};
        project.kicad.clone()
    };
    let destination=previous.as_ref().and_then(|link|managed_root(link).ok()).filter(|root|!root.starts_with(cache.canonicalize().unwrap_or_else(|_|cache.to_owned()).join("repositories"))).unwrap_or_else(||cache.join("repository"));
    let Some(source)=cached_tip(cache,tip) else {return Ok(())};
    // Move the previously attached version instead of discarding edits or making
    // yet another copy. Other historical directories are deliberately retained.
    if !destination.exists() {
        if let Some(link)=&previous {
            let old_versions=cache.join("repositories");
            if let Ok(relative)=Path::new(&link.path).strip_prefix(&old_versions) {
                if let Some(std::path::Component::Normal(commit))=relative.components().next(){
                    let old=old_versions.join(commit);
                    if old.join(".git").is_dir(){
                        let project_path=Path::new(&link.path).strip_prefix(&old)?.to_owned();
                        std::fs::rename(&old,&destination)?;
                        let mut locked=shared.lock().map_err(|_|anyhow::anyhow!("Inventory unavailable"))?;
                        locked.change(|data|{if let Some(project)=data.projects.iter_mut().find(|p|p.id==project_id).filter(|p|eligible(p)){
                            if let Some(link)=&mut project.kicad{link.path=destination.join(&project_path).to_string_lossy().into_owned();}
                        }Ok(())})?;
                    }
                }
            }
        }
    }
    let choices=if destination.is_dir(){
        streaming::repository(&source,&tip.to_hex(),&destination)?
    }else{
        let stage=tempfile::tempdir_in(cache)?;
        let choices=streaming::repository(&source,&tip.to_hex(),stage.path())?;
        std::fs::rename(stage.path(),&destination)?;
        choices
    };
    let mut locked=shared.lock().map_err(|_|anyhow::anyhow!("Inventory unavailable"))?;
    locked.change(|data|{
        if let Some(project)=data.projects.iter_mut().find(|p|p.id==project_id).filter(|p|eligible(p)) {
            let existing=project.kicad.as_ref().map(|link|PathBuf::from(&link.path)).filter(|path|path.is_file()&&path.canonicalize().ok().is_some_and(|path|destination.canonicalize().ok().is_some_and(|destination|path.starts_with(destination))));
            let path=existing.or_else(||choices.first().map(|first|destination.join(first))).context("Bundle has no KiCad project")?;
            let link=project.kicad.get_or_insert_with(Default::default);
            link.path=path.to_string_lossy().into_owned();link.revision=format!("git-snapshot:{project_id}:{}",tip.to_hex());link.files.clear();link.browser_folder=None;
        }
        Ok(())
    })?;
    Ok(())
}

pub(crate) async fn follow(store: WeakStore, mut url: url::Url, grant: uuid::Uuid, wake: std::sync::Arc<tokio::sync::Notify>, peer_id: String) {
    use std::time::Duration;
    url.set_path("/git-bundle");
    let mut cursor = 0;
    let mut tick = tokio::time::interval(Duration::from_secs(8));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! { _ = tick.tick() => {}, _ = wake.notified() => {} }
        let inventory = store.clone();
        let Ok(mut pending) = crate::async_io::blocking(move || {
            let mut pending = pending_bundles(&inventory)?;
            for bundle in &pending {
                if cached_tip(&bundle.cache,bundle.range.tip).is_some(){
                    if let Err(error)=apply_cached_snapshot(&inventory,&bundle.project,bundle.range.tip,&bundle.cache){eprintln!("Git snapshot pending: {error:#}");}
                }
            }
            pending.retain(|bundle| cached_tip(&bundle.cache,bundle.range.tip).is_none()&&!cleanup::applied(bundle));
            Ok(pending)
        })
        .await
        else {
            continue;
        };
        if !pending.is_empty() {
            let index = cursor % pending.len();
            cursor = cursor.wrapping_add(1);
            let bundle=pending.swap_remove(index);
            let project=bundle.project.clone();let tip=bundle.range.tip;let cache=bundle.cache.clone();
            let Ok(trust) = crate::network_tls::peer_trust(&store, &url, Some(&peer_id)) else { break; };
            if receive(&url, grant, bundle, trust).await.is_ok() {
                let apply_store=store.clone();
                if let Err(error)=crate::async_io::blocking(move||apply_cached_snapshot(&apply_store,&project,tip,&cache)).await {eprintln!("Git snapshot pending: {error:#}");}
                announce_ready(&store);
            }
        }
    }
}

// Browser-authored bundles arrive on the bulk endpoint, never the history socket.
async fn receive_upload<S:tokio::io::AsyncRead+tokio::io::AsyncWrite+Unpin+Send>(
    ws: &mut tokio_tungstenite::WebSocketStream<S>,
    store: WeakStore,
    request: Request,
) -> Result<()> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    use tokio::io::AsyncWriteExt;
    let timeout=std::time::Duration::from_secs(60);
    let upload=request.upload.context("Missing upload metadata")?;
    ensure!(upload.size>0 && upload.size<=MAX_TRANSFER,"Invalid bundle upload size");
    ensure!(upload.sha256.len()==64 && upload.sha256.bytes().all(|b| b.is_ascii_hexdigit()),"Invalid bundle checksum");
    let base=request.base.as_deref().map(ObjectId::from_hex).transpose()?;
    let tip=ObjectId::from_hex(request.tip.as_deref().context("Missing upload tip")?)?;
    let lookup=store.clone();let project_id=request.project.clone();
    let (cache,folder,managed)=crate::async_io::blocking(move || {
        let shared=lookup.upgrade().context("Inventory closed")?;let locked=shared.lock().map_err(|_|anyhow::anyhow!("Inventory unavailable"))?;
        let project=locked.data.projects.iter().find(|p|p.id==project_id).context("Project has not arrived yet")?;
        ensure!(project.git_tips.values().any(|t|t.commit==tip.to_hex()),"Commit announcement has not arrived yet");
        let id:uuid::Uuid=project.id.parse()?;
        let cache=locked.path.with_extension("git-bundles").join(id.to_string());
        let managed=project.kicad.as_ref().and_then(|link|managed_root(link).ok()).unwrap_or_else(||cache.join("repository"));
        Ok((cache,project.kicad.as_ref().filter(|link|!link.revision.starts_with("git-snapshot:")).and_then(crate::ui::project_git::folder),managed))
    }).await?;
    tokio::fs::create_dir_all(&cache).await?;
    let temp=tempfile::NamedTempFile::new_in(&cache)?;
    let mut file=tokio::fs::OpenOptions::new().write(true).open(temp.path()).await?;
    ws.send(Message::Text("{\"upload\":true}".into())).await?;
    let mut progress=crate::git_progress::Receive::new(upload.size);
    let mut received=0; let mut acknowledged=0;
    while received<upload.size {
        require_grant(&store,request.grant)?;
        let frame=tokio::time::timeout(timeout,ws.next()).await?.context("Upload interrupted")??;
        let Message::Binary(bytes)=frame else {anyhow::bail!("Expected bundle bytes")};
        received+=bytes.len() as u64;ensure!(bytes.len()<=64*1024 && received<=upload.size,"Invalid upload frame");
        file.write_all(&bytes).await?;
        progress.set(received,upload.size);
        if request.flow && (received-acknowledged>=TRANSFER_WINDOW || received==upload.size){ws.send(Message::Text(serde_json::json!({"received":received}).to_string().into())).await?;acknowledged=received;}
    }
    require_grant(&store,request.grant)?;
    progress.applying();
    file.flush().await?;file.sync_all().await?;drop(file);
    crate::async_io::blocking(move || {
        ensure!(sha256(temp.path())?==upload.sha256,"Bundle checksum mismatch");
        let range=BundleRange{base,tip};open_checked(temp.path(),&range)?;
        if managed.join(".git").is_dir(){
            streaming::receive(temp.path(),&tip.to_hex(),&managed)?;
        } else if let Some(base)=base {
            let previous=cache.join(BundleRange{base:None,tip:base}.file_name());
            ensure!(previous.is_file(),"Bundle base is unavailable; retry with a full bundle");
            streaming::process(&[previous.as_path(),temp.path()],&tip.to_hex(),Some(&cache.join(BundleRange{base:None,tip}.file_name())))?;
        } else { streaming::process(&[temp.path()],&tip.to_hex(),None)?; }
        if let Some(folder)=folder && let Ok(Some(repo))=crate::ui::project_git::open(&folder){let _=import(&repo,temp.path(),&range,&CancelToken::new());}
        let destination=cache.join(range.file_name());
        match temp.persist_noclobber(destination){Ok(_)=>{},Err(e) if e.error.kind()==io::ErrorKind::AlreadyExists=>{},Err(e)=>return Err(e.into())}
        Ok(())
    }).await?;
    let apply_store=store.clone();let project=request.project.clone();
    crate::async_io::blocking(move||{
        let pending=pending_bundles(&apply_store)?;
        for bundle in pending.into_iter().filter(|b|b.project==project){apply_cached_snapshot(&apply_store,&bundle.project,bundle.range.tip,&bundle.cache)?;}
        Ok(())
    }).await?;
    announce_ready(&store);
    ws.send(Message::Text("{\"stored\":true}".into())).await?;
    Ok(())
}

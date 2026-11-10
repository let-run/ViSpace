//! Filesystem-independent local repository operations. The browser supplies a
//! bounded directory snapshot and applies the returned write plan under a lock.

use crate::objects::{
    HashAlgo, ObjectId, ObjectKind, TreeEntry, parse_commit, parse_tree_with_oid_len,
    serialize_tree,
};
use crate::odb::store::{MemoryStore, ObjectStore, WritableObjectStore};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::{Read, Write},
    sync::Arc,
};

const INITIAL_IGNORE_RULES: &str = ".vspace.cache\n.DS_Store\n.history/\n*.lck\n";

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn check(ok: bool, message: &str) -> Result<()> {
    if !ok {
        return Err(std::io::Error::other(message).into());
    }
    Ok(())
}
#[derive(Deserialize)]
/// Bounded directory contents and the requested local operation.
pub struct Input {
    /// Files relative to the granted repository root, including Git metadata.
    pub files: BTreeMap<String, Vec<u8>>,
    /// Commit message, or None to inspect status.
    pub message: Option<String>,
    /// Commit author name.
    pub name: String,
    /// Commit author email.
    pub email: String,
    /// Commit timestamp in Unix seconds.
    pub timestamp: u64,
    /// Initialize an empty repository rather than inspect or commit.
    #[serde(default)]
    pub init: bool,
    /// Received bundle bytes to import without moving the working branch.
    #[serde(default)]
    pub imports: Vec<Vec<u8>>,
    /// Advertised commits whose ancestry should be checked before publication.
    #[serde(default)]
    pub known_tips: Vec<String>,
}
#[derive(Serialize)]
/// Immutable objects, metadata writes and a transferable Git bundle.
pub struct Output {
    /// Files to atomically replace after checking the original branch.
    pub writes: BTreeMap<String, Vec<u8>>,
    /// Previous branch tip.
    pub head: Option<String>,
    /// New branch tip after a commit.
    pub tip: Option<String>,
    /// Fully qualified branch reference.
    pub reference: String,
    /// Changed repository-relative file paths.
    pub changes: Vec<String>,
    /// Full self-contained Git bundle for the new tip.
    pub bundle: Vec<u8>,
    /// Incremental bundle relative to the previous branch tip, when present.
    pub incremental_bundle: Vec<u8>,
    /// Previously advertised tips proven to be ancestors of the new commit.
    pub ancestors: Vec<String>,
}
fn text(files: &BTreeMap<String, Vec<u8>>, path: &str) -> String {
    files
        .get(path)
        .map(|v| String::from_utf8_lossy(v).trim().to_owned())
        .unwrap_or_default()
}
fn walk(store: &MemoryStore, id: ObjectId, seen: &mut HashSet<ObjectId>) -> Result<()> {
    if !seen.insert(id) {
        return Ok(());
    }
    let object = store
        .read(&id)?
        .ok_or_else(|| std::io::Error::other("Missing Git object"))?;
    match object.kind {
        ObjectKind::Commit => {
            let c = parse_commit(&object.data)?;
            walk(store, c.tree, seen)?;
            for p in c.parents {
                walk(store, p, seen)?;
            }
        }
        ObjectKind::Tree => {
            for e in parse_tree_with_oid_len(&object.data, 20)? {
                if e.mode != 0o160000 {
                    walk(store, e.oid, seen)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}
fn put(
    store: &MemoryStore,
    writes: &mut BTreeMap<String, Vec<u8>>,
    kind: ObjectKind,
    data: &[u8],
) -> Result<ObjectId> {
    let oid = store.write(kind, data, Default::default())?;
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    write!(encoder, "{} {}\0", kind.as_str(), data.len())?;
    encoder.write_all(data)?;
    let hex = oid.to_hex();
    writes.insert(
        format!(".git/objects/{}/{}", &hex[..2], &hex[2..]),
        encoder.finish()?,
    );
    Ok(oid)
}
fn tree(
    store: &MemoryStore,
    writes: &mut BTreeMap<String, Vec<u8>>,
    paths: &BTreeMap<String, (ObjectId, u32)>,
    prefix: &str,
) -> Result<ObjectId> {
    let mut entries = Vec::new();
    let mut dirs = HashSet::new();
    for (path, (oid, mode)) in paths {
        if let Some(rest) = path.strip_prefix(prefix) {
            if let Some((dir, _)) = rest.split_once('/') {
                dirs.insert(dir.to_owned());
            } else {
                entries.push(TreeEntry {
                    mode: *mode,
                    name: rest.as_bytes().to_vec(),
                    oid: *oid,
                });
            }
        }
    }
    for dir in dirs {
        let oid = tree(store, writes, paths, &format!("{prefix}{dir}/"))?;
        entries.push(TreeEntry {
            mode: 0o40000,
            name: dir.into_bytes(),
            oid,
        });
    }
    let canonical = crate::objects::write::canonical_tree_entries(entries, HashAlgo::Sha1)?;
    put(store, writes, ObjectKind::Tree, &serialize_tree(&canonical))
}
/// Compute init, status, or commit without accessing native files or the network.
pub fn execute(input: Input) -> Result<Output> {
    check(input.files.len() <= 10000, "Repository exceeds 10000 files")?;
    check(
        input.files.values().map(Vec::len).sum::<usize>() <= 128 * 1024 * 1024,
        "Repository exceeds 128 MiB",
    )?;
    for path in input.files.keys() {
        check(
            !path.starts_with('/')
                && !path.contains('\\')
                && !path.split('/').any(|s| s == ".." || s.is_empty()),
            "Unsafe repository path",
        )?;
    }
    check(
        !input
            .files
            .keys()
            .any(|p| p.ends_with(".lock") && p.starts_with(".git/")),
        "Repository is locked by another Git operation",
    )?;
    check(
        !input.files.contains_key(".git/MERGE_HEAD")
            && !input.files.contains_key(".git/rebase-merge/head-name"),
        "Finish the pending merge or rebase with local Git first",
    )?;
    check(
        !input.files.contains_key(".git") && !input.files.contains_key(".git/commondir"),
        "Linked worktrees are not supported in the browser yet",
    )?;
    let initialized = input.files.contains_key(".git/HEAD");
    check(initialized || input.init, "Initialize the repository first")?;
    check(
        !initialized || !input.init,
        "A Git repository already exists",
    )?;
    let mut writes = BTreeMap::new();
    if input.init {
        writes.insert(".git/HEAD".into(), b"ref: refs/heads/main\n".to_vec());
        writes.insert(
            ".git/config".into(),
            b"[core]\n\trepositoryformatversion = 0\n\tbare = false\n\tfilemode = false\n".to_vec(),
        );
        let mut ignore = text(&input.files, ".gitignore");
        for item in INITIAL_IGNORE_RULES.lines() {
            if !ignore.lines().any(|l| l.trim() == item) {
                if !ignore.is_empty() {
                    ignore.push('\n');
                }
                ignore.push_str(item);
            }
        }
        ignore.push('\n');
        writes.insert(".gitignore".into(), ignore.into_bytes());
        return Ok(Output {
            writes,
            head: None,
            tip: None,
            reference: "refs/heads/main".into(),
            changes: vec![],
            bundle: vec![],
            incremental_bundle: vec![],
            ancestors: vec![],
        });
    }
    check(
        !text(&input.files, ".git/config")
            .to_lowercase()
            .contains("objectformat = sha256"),
        "SHA-256 repositories are not supported by the browser adapter yet",
    )?;
    let head_text = text(&input.files, ".git/HEAD");
    let reference = head_text
        .strip_prefix("ref: ")
        .ok_or_else(|| {
            std::io::Error::other("Check out a branch with local Git before committing")
        })?
        .to_owned();
    check(
        reference.starts_with("refs/heads/") && !reference.contains(".."),
        "Invalid branch reference",
    )?;
    let mut head = text(&input.files, &format!(".git/{reference}"));
    if head.is_empty() {
        for line in text(&input.files, ".git/packed-refs").lines() {
            if let Some((id, name)) = line.split_once(' ') {
                if name == reference {
                    head = id.to_owned();
                }
            }
        }
    }
    let parent = if head.is_empty() {
        None
    } else {
        Some(ObjectId::from_hex(&head)?)
    };
    let store = Arc::new(MemoryStore::new(HashAlgo::Sha1));
    for (path, bytes) in &input.files {
        if path.starts_with(".git/objects/pack/") && path.ends_with(".pack") {
            crate::objects::unpack_objects::ingest_pack_into(
                &mut std::io::Cursor::new(bytes),
                store.as_ref(),
                store.as_ref(),
                &Default::default(),
            )?;
        } else if let Some(rest) = path.strip_prefix(".git/objects/") {
            let hex = rest.replace('/', "");
            if hex.len() != 40 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                continue;
            }
            let mut raw = Vec::new();
            flate2::read::ZlibDecoder::new(bytes.as_slice())
                .take(32 * 1024 * 1024 + 1)
                .read_to_end(&mut raw)?;
            check(raw.len() <= 32 * 1024 * 1024, "Git object exceeds 32 MiB")?;
            let split = raw
                .iter()
                .position(|b| *b == 0)
                .ok_or_else(|| std::io::Error::other("Invalid loose object"))?;
            let header = String::from_utf8_lossy(&raw[..split]);
            let (kind, size) = header
                .split_once(' ')
                .ok_or_else(|| std::io::Error::other("Invalid object header"))?;
            let kind = match kind {
                "blob" => ObjectKind::Blob,
                "tree" => ObjectKind::Tree,
                "commit" => ObjectKind::Commit,
                "tag" => ObjectKind::Tag,
                _ => return Err(std::io::Error::other("Invalid object kind").into()),
            };
            check(
                size.parse::<usize>()? == raw.len() - split - 1,
                "Invalid object size",
            )?;
            check(
                store
                    .write(kind, &raw[split + 1..], Default::default())?
                    .to_hex()
                    == hex,
                "Corrupt loose object",
            )?;
        }
    }
    check(
        !input.files.contains_key(".git/reftable/tables.list"),
        "Reftable repositories require local Git",
    )?;
    check(
        !input.files.keys().any(|p| {
            p.ends_with(".gitattributes")
                && String::from_utf8_lossy(&input.files[p]).contains("filter=")
        }),
        "Use local Git for repositories with content filters such as Git LFS",
    )?;
    if !input.imports.is_empty() {
        check(
            input.imports.len() <= 16
                && input.imports.iter().map(Vec::len).sum::<usize>() <= 128 * 1024 * 1024,
            "Received bundle set exceeds browser limits",
        )?;
        let mut original = HashSet::new();
        store.for_each_object(&mut |id| {
            original.insert(*id);
            std::ops::ControlFlow::Continue(())
        })?;
        let mut pending = input.imports.iter().collect::<Vec<_>>();
        let mut imported = Vec::new();
        while !pending.is_empty() {
            let before = pending.len();
            let mut retry = Vec::new();
            for bytes in pending {
                let mut reader = std::io::BufReader::new(std::io::Cursor::new(bytes));
                let header = crate::bundle::read_header(&mut reader)?;
                check(
                    header.object_format == HashAlgo::Sha1,
                    "Bundle uses a different Git object format",
                )?;
                if header
                    .prerequisites
                    .iter()
                    .any(|(id, _)| store.read(id).ok().flatten().is_none())
                {
                    retry.push(bytes);
                    continue;
                }
                crate::objects::unpack_objects::ingest_pack_into(
                    &mut reader,
                    store.as_ref(),
                    store.as_ref(),
                    &crate::objects::unpack_objects::UnpackOptions {
                        strict: true,
                        max_input_bytes: Some(128 * 1024 * 1024),
                        ..Default::default()
                    },
                )?;
                for (_, id) in &header.refs {
                    walk(&store, *id, &mut HashSet::new())?;
                    imported.push(*id);
                }
            }
            check(
                retry.len() < before,
                "Missing prerequisite for received bundles",
            )?;
            pending = retry;
        }
        let mut objects = Vec::new();
        store.for_each_object(&mut |id| {
            if !original.contains(id) {
                objects.push(*id);
            }
            std::ops::ControlFlow::Continue(())
        })?;
        for id in objects {
            let object = store
                .read(&id)?
                .ok_or_else(|| std::io::Error::other("Missing imported object"))?;
            put(&store, &mut writes, object.kind, &object.data)?;
        }
        for id in imported {
            writes.insert(
                format!(".git/refs/vinventory/bundles/{}", id.to_hex()),
                format!("{}\n", id.to_hex()).into_bytes(),
            );
        }
        return Ok(Output {
            writes,
            head: parent.map(|p| p.to_hex()),
            tip: None,
            reference,
            changes: vec![],
            bundle: vec![],
            incremental_bundle: vec![],
            ancestors: vec![],
        });
    }
    let previous = if let Some(bytes) = input.files.get(".git/index") {
        crate::index::Index::parse(bytes)?
    } else {
        crate::index::Index::new()
    };
    check(
        !previous.has_unmerged_entries(),
        "Resolve unmerged index entries with local Git first",
    )?;
    check(
        !previous.entries().iter().any(|e| e.skip_worktree()),
        "Use local Git for sparse checkouts",
    )?;
    let tracked: HashMap<_, _> = previous
        .entries()
        .iter()
        .map(|e| (String::from_utf8_lossy(&e.path).into_owned(), e.clone()))
        .collect();
    check(
        !tracked
            .values()
            .any(|e| e.mode == 0o120000 || e.mode == 0o160000),
        "Browser commits currently require regular files; use local Git for symlinks or submodules",
    )?;
    let mut rules: Vec<_> = input
        .files
        .iter()
        .filter(|(p, _)| p.as_str() == ".gitignore" || p.ends_with("/.gitignore"))
        .map(|(p, v)| {
            crate::ignore::IgnorePatternList::parse(
                v,
                p.rsplit_once('/').map_or("", |(dir, _)| dir),
            )
        })
        .collect();
    rules.insert(
        0,
        crate::ignore::IgnorePatternList::parse(
            input
                .files
                .get(".git/info/exclude")
                .map_or(&[][..], Vec::as_slice),
            "",
        ),
    );
    let mut paths = BTreeMap::new();
    let mut index = crate::index::Index::new();
    let mut changes = Vec::new();
    for (path, bytes) in &input.files {
        if path == ".git" || path.starts_with(".git/") {
            continue;
        }
        let ignored = rules
            .iter()
            .filter_map(|r| r.is_ignored(path, false, false))
            .last()
            .unwrap_or(false)
            || path.match_indices('/').any(|(end, _)| {
                rules
                    .iter()
                    .filter_map(|r| r.is_ignored(&path[..end], true, false))
                    .last()
                    .unwrap_or(false)
            });
        if ignored && !tracked.contains_key(path) {
            continue;
        }
        let oid = put(&store, &mut writes, ObjectKind::Blob, bytes)?;
        let mode = tracked.get(path).map_or(0o100644, |e| e.mode);
        paths.insert(path.clone(), (oid, mode));
        if tracked.get(path).is_none_or(|e| e.oid != oid) {
            changes.push(path.clone());
        }
        index.entries_mut().push(crate::index::IndexEntry {
            ctime_sec: 0,
            ctime_nsec: 0,
            mtime_sec: 0,
            mtime_nsec: 0,
            dev: 0,
            ino: 0,
            mode,
            uid: 0,
            gid: 0,
            size: bytes.len() as u32,
            oid,
            flags: 0,
            flags_extended: None,
            path: path.as_bytes().to_vec(),
            base_index_pos: 0,
        });
    }
    for path in tracked.keys() {
        if !paths.contains_key(path) {
            changes.push(path.clone());
        }
    }
    let root = tree(&store, &mut writes, &paths, "")?;
    if let Some(parent) = parent {
        let old = store
            .read(&parent)?
            .ok_or_else(|| std::io::Error::other("HEAD commit is missing"))?;
        if parse_commit(&old.data)?.tree == root {
            changes.clear();
        }
    }
    let Some(message) = input.message else {
        return Ok(Output {
            writes: BTreeMap::new(),
            head: parent.map(|p| p.to_hex()),
            tip: None,
            reference,
            changes,
            bundle: vec![],
            incremental_bundle: vec![],
            ancestors: vec![],
        });
    };
    check(!message.trim().is_empty(), "Enter a commit message")?;
    check(!changes.is_empty() || parent.is_none(), "Nothing to commit")?;
    for identity in [&input.name, &input.email] {
        check(
            !identity.trim().is_empty()
                && !identity
                    .chars()
                    .any(|c| c.is_control() || c == '<' || c == '>'),
            "Enter a valid Git author name and email",
        )?;
    }
    let signature = format!("{} <{}> {} +0000", input.name, input.email, input.timestamp);
    let data = crate::objects::CommitData {
        tree: root,
        parents: parent.into_iter().collect(),
        author: signature.clone(),
        committer: signature,
        author_raw: vec![],
        committer_raw: vec![],
        encoding: None,
        message: format!("{}\n", message.trim()),
        raw_message: None,
        extra_headers: vec![],
    };
    let tip = put(
        &store,
        &mut writes,
        ObjectKind::Commit,
        &crate::objects::serialize_commit(&data),
    )?;
    index.sort();
    let mut encoded = Vec::new();
    index.serialize_into(&mut encoded)?;
    let mut hasher = crate::hash::ChecksumHasher::new(HashAlgo::Sha1);
    hasher.update(&encoded);
    encoded.extend(hasher.finalize());
    writes.insert(".git/index".into(), encoded);
    writes.insert(
        format!(".git/{reference}"),
        format!("{}\n", tip.to_hex()).into_bytes(),
    );
    let mut reachable = HashSet::new();
    walk(&store, tip, &mut reachable)?;
    let odb = crate::odb::OdbBuilder::files("")
        .primary(store.clone())
        .alternates(false)
        .build();
    let pack = crate::pack_objects::build_pack_from_send_list(
        &odb,
        &reachable.iter().copied().collect::<Vec<_>>(),
        &HashSet::new(),
        &crate::pack_objects::PackBuildOptions {
            reuse_objects: false,
            reuse_deltas: false,
            use_bitmaps: false,
            ..Default::default()
        },
        &HashMap::new(),
    )
    .map_err(|e| std::io::Error::other(format!("Bundle creation: {e}")))?;
    let mut bundle = format!("# v2 git bundle\n{} {}\n\n", tip.to_hex(), reference).into_bytes();
    bundle.extend(pack);
    let mut incremental_bundle = Vec::new();
    if let Some(base) = parent {
        let mut excluded = HashSet::new();
        walk(&store, base, &mut excluded)?;
        let send = reachable.difference(&excluded).copied().collect::<Vec<_>>();
        let pack = crate::pack_objects::build_pack_from_send_list(
            &odb,
            &send,
            &excluded,
            &crate::pack_objects::PackBuildOptions {
                reuse_objects: false,
                reuse_deltas: false,
                use_bitmaps: false,
                ..Default::default()
            },
            &HashMap::new(),
        )?;
        incremental_bundle = format!(
            "# v2 git bundle\n-{} previous tip\n{} {}\n\n",
            base.to_hex(),
            tip.to_hex(),
            reference
        )
        .into_bytes();
        incremental_bundle.extend(pack);
    }
    Ok(Output {
        writes,
        head: parent.map(|p| p.to_hex()),
        tip: Some(tip.to_hex()),
        reference,
        changes,
        bundle,
        incremental_bundle,
        ancestors: input
            .known_tips
            .into_iter()
            .filter(|id| {
                ObjectId::from_hex(id).is_ok_and(|id| reachable.contains(&id) && id != tip)
            })
            .collect(),
    })
}

/// Select directory entries before loading file contents, respecting tracked
/// paths and nested Git ignore rules. Git metadata is always included.
pub fn select_files(
    index: Option<&[u8]>,
    ignores: BTreeMap<String, Vec<u8>>,
    paths: Vec<String>,
    init: bool,
) -> Result<Vec<String>> {
    let index = if let Some(bytes) = index {
        crate::index::Index::parse(bytes)?
    } else {
        crate::index::Index::new()
    };
    let tracked: HashSet<_> = index
        .entries()
        .iter()
        .map(|e| String::from_utf8_lossy(&e.path).into_owned())
        .collect();
    let mut rules = Vec::new();
    if let Some(bytes) = ignores.get(".git/info/exclude") {
        rules.push(crate::ignore::IgnorePatternList::parse(bytes, ""));
    }
    for (path, bytes) in &ignores {
        if path == ".gitignore" || path.ends_with("/.gitignore") {
            rules.push(crate::ignore::IgnorePatternList::parse(
                bytes,
                path.rsplit_once('/').map_or("", |(dir, _)| dir),
            ));
        }
    }
    if init {
        rules.push(crate::ignore::IgnorePatternList::parse(
            INITIAL_IGNORE_RULES.as_bytes(),
            "",
        ));
    }
    Ok(paths
        .into_iter()
        .filter(|path| {
            if path == ".git" || path.starts_with(".git/") || tracked.contains(path) {
                return true;
            }
            let ignored = rules
                .iter()
                .filter_map(|r| r.is_ignored(path, false, false))
                .last()
                .unwrap_or(false)
                || path.match_indices('/').any(|(end, _)| {
                    rules
                        .iter()
                        .filter_map(|r| r.is_ignored(&path[..end], true, false))
                        .last()
                        .unwrap_or(false)
                });
            !ignored
        })
        .collect())
}

/// Bounded-memory directory and bundle operations.
pub mod stream;

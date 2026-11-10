//! Bundle ingestion in a dedicated browser worker. Git transport is supplied by the caller.
use anyhow::{Context, Result, ensure};
use grit_lib::{
    bundle::{BundleHeader, read_header},
    objects::{
        ObjectId, ObjectKind,
        odb::store::{MemoryStore, ObjectStore},
        parse_commit, parse_tree_with_oid_len,
        unpack_objects::{UnpackOptions, ingest_pack_into},
    },
};
use std::{cell::RefCell, collections::BTreeMap, io::BufReader};
use wasm_bindgen::prelude::*;

const MAX_BUNDLE_BYTES: usize = 128 * 1024 * 1024;
const MAX_PROJECT_BYTES: usize = 64 * 1024 * 1024;
const MAX_FILE_BYTES: usize = 32 * 1024 * 1024;

thread_local! {
    static STORE: RefCell<Option<MemoryStore>> = const { RefCell::new(None) };
}

fn header(bytes: &[u8]) -> Result<BundleHeader> {
    Ok(read_header(&mut BufReader::new(bytes))?)
}

#[wasm_bindgen]
pub fn git_bundle_header(bytes: &[u8]) -> Result<String, String> {
    let header = header(bytes).map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "bases": header.prerequisites.iter().map(|(id, _)| id.to_hex()).collect::<Vec<_>>(),
        "tips": header.refs.iter().map(|(name, id)| (name, id.to_hex())).collect::<BTreeMap<_, _>>(),
    }).to_string())
}

#[wasm_bindgen]
pub fn ingest_git_bundle(bytes: &[u8]) -> Result<(), String> {
    STORE.with_borrow_mut(|slot| {
        (|| -> Result<()> {
            ensure!(
                bytes.len() <= MAX_BUNDLE_BYTES,
                "Bundle exceeds the 128 MiB browser limit"
            );
            let mut reader = BufReader::new(bytes);
            let header = read_header(&mut reader)?;
            let store = slot.get_or_insert_with(|| MemoryStore::new(header.object_format));
            ensure!(
                store.hash_algo() == header.object_format,
                "Bundle object formats differ"
            );
            for (id, _) in &header.prerequisites {
                ensure!(
                    store
                        .read(id)?
                        .is_some_and(|object| object.kind == ObjectKind::Commit),
                    "Bundle requires commit {}",
                    id.to_hex()
                );
            }
            let opts = UnpackOptions {
                strict: true,
                max_input_bytes: Some(MAX_BUNDLE_BYTES as u64),
                ..Default::default()
            };
            ingest_pack_into(&mut reader, store, store, &opts)?;
            Ok(())
        })()
        .map_err(|e| e.to_string())
    })
}

#[wasm_bindgen]
pub fn kicad_files_at_git_commit(tip: &str) -> Result<String, String> {
    STORE.with_borrow(|slot| {
        (|| -> Result<String> {
            let store = slot.as_ref().context("No Git bundles loaded")?;
            let tip = ObjectId::from_hex(tip)?;
            let object = store
                .read(&tip)?
                .context("Commit is missing from the bundle")?;
            ensure!(
                object.kind == ObjectKind::Commit,
                "Bundle tip is not a commit"
            );
            let commit = parse_commit(&object.data)?;
            let mut files = BTreeMap::new();
            let mut total = 0;
            visit_tree(store, commit.tree, "", 0, &mut files, &mut total)?;
            let choices = files
                .keys()
                .filter(|path| path.ends_with(".kicad_pro"))
                .cloned()
                .collect::<Vec<_>>();
            ensure!(
                !choices.is_empty(),
                "The commit contains no KiCad project files"
            );
            Ok(serde_json::json!({"choices":choices,"files":files}).to_string())
        })()
        .map_err(|e| e.to_string())
    })
}

fn visit_tree(
    store: &MemoryStore,
    tree: ObjectId,
    prefix: &str,
    depth: usize,
    files: &mut BTreeMap<String, String>,
    total: &mut usize,
) -> Result<()> {
    ensure!(depth <= 32, "Git tree nesting exceeds 32 levels");
    let object = store.read(&tree)?.context("Git tree is missing")?;
    ensure!(object.kind == ObjectKind::Tree, "Expected a Git tree");
    for entry in parse_tree_with_oid_len(&object.data, store.hash_algo().len())? {
        let Ok(name) = std::str::from_utf8(&entry.name) else {
            continue;
        };
        if name.is_empty() || name.starts_with('.') || name.contains(['/', '\\', '\0']) {
            continue;
        }
        let path = format!("{prefix}{name}");
        if entry.mode == 0o040000 {
            if !name.ends_with("-backups") {
                visit_tree(
                    store,
                    entry.oid,
                    &format!("{path}/"),
                    depth + 1,
                    files,
                    total,
                )?;
            }
        } else if matches!(entry.mode, 0o100644 | 0o100755)
            && name.rsplit_once('.').is_some_and(|(_, ext)| {
                matches!(ext, "kicad_pro" | "kicad_sch" | "kicad_pcb" | "kicad_dru")
            })
        {
            let object = store
                .read(&entry.oid)?
                .context("KiCad file object is missing")?;
            ensure!(object.kind == ObjectKind::Blob, "Expected a Git blob");
            *total += object.data.len();
            ensure!(
                files.len() < 512
                    && object.data.len() <= MAX_FILE_BYTES
                    && *total <= MAX_PROJECT_BYTES,
                "KiCad files exceed the browser project limits"
            );
            files.insert(path, String::from_utf8(object.data)?);
        }
    }
    Ok(())
}

#[wasm_bindgen]
pub fn browser_repository_job(input: &str) -> Result<String, String> {
    let input = serde_json::from_str(input).map_err(|e| format!("Invalid repository input: {e}"))?;
    let result = grit_lib::browser_repository::execute(input).map_err(|e| e.to_string())?;
    serde_json::to_string(&result).map_err(|e| e.to_string())
}

#[wasm_bindgen]
pub fn browser_repository_paths(input: &str) -> Result<String, String> {
    #[derive(serde::Deserialize)]
    struct Paths {
        index: Option<Vec<u8>>,
        ignores: BTreeMap<String,Vec<u8>>,
        paths: Vec<String>,
        init: bool,
    }
    let input:Paths=serde_json::from_str(input).map_err(|e|e.to_string())?;
    let paths=grit_lib::browser_repository::select_files(input.index.as_deref(),input.ignores,input.paths,input.init).map_err(|e|e.to_string())?;
    serde_json::to_string(&paths).map_err(|e|e.to_string())
}

#[wasm_bindgen(module = "/web/git-io.js")]
extern "C" {
    #[wasm_bindgen(catch,js_name=gitRead)]
    fn stream_read(path: Option<String>, offset:f64, length:u32)->Result<js_sys::Uint8Array,JsValue>;
    #[wasm_bindgen(catch,js_name=gitAppend)]
    fn stream_append(bytes:&[u8])->Result<f64,JsValue>;
    #[wasm_bindgen(catch,js_name=gitJournalLength)]
    fn stream_length()->Result<f64,JsValue>;
}
#[derive(Debug)]
struct BrowserStreamIo;
fn stream_error(error:JsValue)->std::io::Error{std::io::Error::other(format!("Browser Git storage: {error:?}"))}
impl grit_lib::browser_repository::stream::Io for BrowserStreamIo {
    fn read_at(&self,path:Option<&str>,offset:u64,out:&mut[u8])->std::io::Result<usize>{
        let bytes=stream_read(path.map(str::to_owned),offset as f64,out.len()as u32).map_err(stream_error)?;
        let n=bytes.length()as usize;if n>out.len(){return Err(std::io::Error::other("Invalid Git read length"))}bytes.copy_to(&mut out[..n]);Ok(n)
    }
    fn append(&self,bytes:&[u8])->std::io::Result<u64>{Ok(stream_append(bytes).map_err(stream_error)? as u64)}
    fn journal_len(&self)->std::io::Result<u64>{Ok(stream_length().map_err(stream_error)?as u64)}
}
#[wasm_bindgen]
pub fn browser_stream_repository(input:&str)->Result<String,String>{
    let input=serde_json::from_str(input).map_err(|e|format!("Invalid Git job: {e}"))?;
    let output=grit_lib::browser_repository::stream::execute(std::sync::Arc::new(BrowserStreamIo),input).map_err(|e|e.to_string())?;
    serde_json::to_string(&output).map_err(|e|e.to_string())
}
#[wasm_bindgen]
pub fn browser_git_heap_bytes()->f64{(core::arch::wasm32::memory_size(0)*65536)as f64}

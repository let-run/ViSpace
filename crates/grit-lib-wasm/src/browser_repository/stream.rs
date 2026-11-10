//! Chunked Git storage for browser directory handles and native bulk receivers.
//! Payloads live in source files or an append-only disk journal, never a repository-sized Vec.
use super::{Result, check};
use crate::objects::{
    HashAlgo, ObjectId, ObjectKind, TreeEntry, parse_commit, parse_tree_with_oid_len,
    serialize_tree,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    io::{self, Read, Seek, SeekFrom, Write},
    sync::Arc,
};
/// Maximum payload buffer passed to the storage adapter.
pub const CHUNK: usize = 64 * 1024;
const META: u64 = 32 * 1024 * 1024;
const OBJECTS: usize = 250_000;
fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}
/// Synchronous, worker-only random reads and append-only staging. `None` names the journal.
pub trait Io: std::fmt::Debug + Send + Sync {
    /// Read at most `buffer.len()` bytes at an absolute offset.
    fn read_at(&self, key: Option<&str>, offset: u64, buffer: &mut [u8]) -> io::Result<usize>;
    /// Append bytes to the journal, returning their absolute starting offset.
    fn append(&self, bytes: &[u8]) -> io::Result<u64>;
    /// Current journal length.
    fn journal_len(&self) -> io::Result<u64>;
}
/// Metadata for a source file. Contents remain in its directory handle.
#[derive(Clone, Deserialize, Serialize, Default)]
pub struct FileMeta {
    /// Byte length.
    pub size: u64,
    /// Last modification time in milliseconds, used for the Git index stat cache.
    #[serde(default)]
    pub modified: u64,
}
/// A byte range in the disk journal.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Range {
    /// Starting byte offset.
    pub offset: u64,
    /// Number of bytes.
    pub length: u64,
    /// SHA-256 for transferable bundle ranges.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}
/// Worker job metadata; no file payloads cross the JSON boundary.
#[derive(Deserialize)]
pub struct Input {
    /// Relative path to immutable source-file metadata.
    pub files: BTreeMap<String, FileMeta>,
    /// `init`, `status`, `commit`, `import`, `extract`, `bundle`, or `hash`.
    pub operation: String,
    /// Selected bundle file keys, in preferred order.
    #[serde(default)]
    pub imports: Vec<String>,
    /// Cached bundles available to satisfy prerequisites on demand.
    #[serde(default)]
    pub cached: Vec<String>,
    /// Expected commit for extraction or bundle validation.
    #[serde(default)]
    pub tip: Option<String>,
    /// Last applied HEAD for automatic checkout safety.
    #[serde(default)]
    pub checkout_head:Option<String>,
    /// Resume an interrupted initial checkout only after verifying all existing files.
    #[serde(default)]
    pub recover_checkout:bool,
    /// Expected SHA-256 of the last selected bundle.
    #[serde(default)]
    pub expected_sha256: Option<String>,
    /// Commit message.
    #[serde(default)]
    pub message: String,
    /// Commit author name.
    #[serde(default)]
    pub name: String,
    /// Commit author email.
    #[serde(default)]
    pub email: String,
    /// Unix timestamp.
    #[serde(default)]
    pub timestamp: u64,
    /// Previously advertised tips to check for ancestry.
    #[serde(default)]
    pub known_tips: Vec<String>,
    /// Number of recent commits requested by the history viewer.
    #[serde(default = "default_history_limit")]
    pub history_limit: usize,
}
fn default_history_limit()->usize{100}
/// A changed working-copy path.
#[derive(Serialize)]
pub struct ChangeDetail {
    /// Repository-relative path.
    pub path:String,
    /// Added, Modified, or Removed.
    pub kind:&'static str,
    /// Bounded text diff counts; omitted for binary or large files.
    pub lines:Option<(usize,usize)>,
}
/// Disk-backed result. The caller streams these ranges to their final destinations.
#[derive(Serialize, Default)]
pub struct Output {
    /// Repository writes, with objects published before index and branch.
    pub writes: BTreeMap<String, Range>,
    /// Tracked files removed by an automatic checkout.
    pub removes:Vec<String>,
    /// Previous branch tip.
    pub head: Option<String>,
    /// New branch tip, if committed.
    pub tip: Option<String>,
    /// Branch reference.
    pub reference: String,
    /// Changed paths.
    pub changes: Vec<String>,
    /// Paths with added/modified/removed status for compact commit review.
    pub change_details:Vec<ChangeDetail>,
    /// Recent commits for the project Git history dialog.
    pub history:Vec<serde_json::Value>,
    /// Complete bundle.
    pub bundle: Option<Range>,
    /// Incremental bundle relative to `head`.
    pub incremental_bundle: Option<Range>,
    /// Proven advertised ancestors.
    pub ancestors: Vec<String>,
    /// KiCad files extracted from the selected commit.
    pub snapshot: BTreeMap<String, Range>,
    /// Project-file choices in that snapshot.
    pub choices: Vec<String>,
    /// Actual selected commit.
    pub selected_tip: Option<String>,
    /// Content hash for `hash` jobs.
    pub checksum: Option<String>,
    /// Whether a local repository exists.
    pub initialized: bool,
    /// Metadata for the selected bundle ranges.
    pub ranges: Vec<BundleRange>,
}
/// Bundle prerequisite and tip metadata, kept separate from payload bytes.
#[derive(Serialize, Clone)]
pub struct BundleRange {
    /// Index within the selected bundle list.
    pub index: usize,
    /// Required commits.
    pub bases: Vec<String>,
    /// Exported commits.
    pub tips: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Location {
    key: Option<String>,
    offset: u64,
    length: u64,
}
impl Location {
    fn source(key: &str, length: u64) -> Self {
        Self {
            key: Some(key.into()),
            offset: 0,
            length,
        }
    }
    fn journal(r: &Range) -> Self {
        Self {
            key: None,
            offset: r.offset,
            length: r.length,
        }
    }
}
struct FileReader {
    io: Arc<dyn Io>,
    loc: Location,
    pos: u64,
}
impl FileReader {
    fn new(io: Arc<dyn Io>, loc: Location) -> Self {
        Self { io, loc, pos: 0 }
    }
}
impl Read for FileReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = (out.len() as u64)
            .min(CHUNK as u64)
            .min(self.loc.length - self.pos) as usize;
        if n == 0 {
            return Ok(0);
        }
        let got = self.io.read_at(
            self.loc.key.as_deref(),
            self.loc.offset + self.pos,
            &mut out[..n],
        )?;
        if got == 0 {
            return Err(invalid("Source file ended early"));
        }
        self.pos += got as u64;
        Ok(got)
    }
}
impl Seek for FileReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let n = match pos {
            SeekFrom::Start(v) => v as i128,
            SeekFrom::Current(v) => self.pos as i128 + v as i128,
            SeekFrom::End(v) => self.loc.length as i128 + v as i128,
        };
        if n < 0 || n > self.loc.length as i128 {
            return Err(invalid("File seek outside bounds"));
        }
        self.pos = n as u64;
        Ok(self.pos)
    }
}
struct Journal {
    io: Arc<dyn Io>,
    start: u64,
    hash: Option<Sha256>,
}
impl Journal {
    fn new(io: Arc<dyn Io>, hash: bool) -> io::Result<Self> {
        Ok(Self {
            start: io.journal_len()?,
            io,
            hash: hash.then(Sha256::new),
        })
    }
    fn finish(self) -> io::Result<Range> {
        Ok(Range {
            offset: self.start,
            length: self.io.journal_len()? - self.start,
            sha256: self
                .hash
                .map(|h| h.finalize().iter().map(|b| format!("{b:02x}")).collect()),
        })
    }
}
impl Write for Journal {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        for chunk in bytes.chunks(CHUNK) {
            self.io.append(chunk)?;
            if let Some(hash) = &mut self.hash {
                hash.update(chunk)
            }
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn stage(io: Arc<dyn Io>, bytes: &[u8]) -> io::Result<Range> {
    let mut w = Journal::new(io, false)?;
    w.write_all(bytes)?;
    w.finish()
}
fn byte(reader: &mut dyn Read) -> io::Result<u8> {
    let mut b = [0];
    reader.read_exact(&mut b)?;
    Ok(b[0])
}
fn varint(reader: &mut dyn Read) -> io::Result<u64> {
    let mut out = 0u64;
    for shift in (0..64).step_by(7) {
        let b = byte(reader)?;
        if shift == 63 && b & 0x7f > 1 {
            return Err(invalid("Git varint overflow"));
        }
        out |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Ok(out);
        }
    }
    Err(invalid("Git varint too long"))
}
fn kind(code: u8) -> io::Result<ObjectKind> {
    match code {
        1 => Ok(ObjectKind::Commit),
        2 => Ok(ObjectKind::Tree),
        3 => Ok(ObjectKind::Blob),
        4 => Ok(ObjectKind::Tag),
        _ => Err(invalid("Invalid Git object kind")),
    }
}
fn pack_header(reader: &mut dyn Read) -> io::Result<(u8, u64)> {
    let first = byte(reader)?;
    let code = (first >> 4) & 7;
    let mut size = (first & 15) as u64;
    let mut b = first;
    let mut shift = 4;
    while b & 128 != 0 {
        b = byte(reader)?;
        if shift > 60 {
            return Err(invalid("Packed object size overflow"));
        }
        size |= ((b & 127) as u64) << shift;
        shift += 7;
    }
    Ok((code, size))
}
fn ofs_base(reader: &mut dyn Read, offset: u64) -> io::Result<u64> {
    let mut b = byte(reader)?;
    let mut distance = (b & 127) as u64;
    for _ in 0..10 {
        if b & 128 == 0 {
            return offset
                .checked_sub(distance)
                .filter(|n| *n >= 12)
                .ok_or_else(|| invalid("Invalid delta base offset"));
        }
        b = byte(reader)?;
        distance = distance
            .checked_add(1)
            .and_then(|v| v.checked_mul(128))
            .and_then(|v| v.checked_add((b & 127) as u64))
            .ok_or_else(|| invalid("Delta offset overflow"))?;
    }
    Err(invalid("Delta offset too long"))
}
#[derive(Clone)]
enum Body {
    Raw(Location),
    Zlib(Location, u64),
    Delta {
        base: Arc<Source>,
        instructions: Location,
    },
}
#[derive(Clone)]
struct Source {
    io: Arc<dyn Io>,
    kind: ObjectKind,
    size: u64,
    body: Body,
}
impl Source {
    fn open(self: &Arc<Self>) -> io::Result<Box<dyn Read + Send>> {
        match &self.body {
            Body::Raw(loc) => Ok(Box::new(FileReader::new(self.io.clone(), loc.clone()))),
            Body::Zlib(loc, skip) => {
                let mut reader =
                    flate2::read::ZlibDecoder::new(FileReader::new(self.io.clone(), loc.clone()));
                let n = io::copy(&mut reader.by_ref().take(*skip), &mut io::sink())?;
                if n != *skip {
                    return Err(invalid("Truncated loose object header"));
                }
                Ok(Box::new(reader))
            }
            Body::Delta { base, instructions } => {
                let mut instructions: Box<dyn Read + Send> =
                    Box::new(flate2::read::ZlibDecoder::new(FileReader::new(
                        self.io.clone(),
                        instructions.clone(),
                    )));
                if varint(&mut instructions)? != base.size
                    || varint(&mut instructions)? != self.size
                {
                    return Err(invalid("Delta size mismatch"));
                }
                Ok(Box::new(DeltaReader {
                    base: PositionReader::new(base.clone())?,
                    instructions,
                    size: self.size,
                    pos: 0,
                    copy: 0,
                    copy_at: 0,
                    literal: 0,
                    finished: false,
                }))
            }
        }
    }
}
struct PositionReader {
    source: Arc<Source>,
    reader: Box<dyn Read + Send>,
    pos: u64,
}
impl PositionReader {
    fn new(source: Arc<Source>) -> io::Result<Self> {
        Ok(Self {
            reader: source.open()?,
            source,
            pos: 0,
        })
    }
    fn seek_to(&mut self, pos: u64) -> io::Result<()> {
        if pos > self.source.size {
            return Err(invalid("Delta copy outside base"));
        }
        if let Body::Raw(loc)=&self.source.body{
            let mut loc=loc.clone();loc.offset+=pos;loc.length-=pos;
            self.reader=Box::new(FileReader::new(self.source.io.clone(),loc));self.pos=pos;return Ok(());
        }
        if pos < self.pos {
            self.reader = self.source.open()?;
            self.pos = 0;
        }
        let n = io::copy(
            &mut self.reader.by_ref().take(pos - self.pos),
            &mut io::sink(),
        )?;
        self.pos += n;
        if self.pos != pos {
            return Err(invalid("Truncated delta base"));
        }
        Ok(())
    }
}
impl Read for PositionReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = self.reader.read(out)?;
        self.pos += n as u64;
        Ok(n)
    }
}
struct DeltaReader {
    base: PositionReader,
    instructions: Box<dyn Read + Send>,
    size: u64,
    pos: u64,
    copy: u64,
    copy_at: u64,
    literal: u64,
    finished: bool,
}
impl Read for DeltaReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        if self.pos == self.size {
            if !self.finished {
                let mut b = [0];
                if self.copy != 0 || self.literal != 0 || self.instructions.read(&mut b)? != 0 {
                    return Err(invalid("Extra delta output"));
                }
                self.finished = true;
            }
            return Ok(0);
        }
        loop {
            if self.copy > 0 {
                self.base.seek_to(self.copy_at)?;
                let limit = out
                    .len()
                    .min(CHUNK)
                    .min(self.copy.min(self.size - self.pos).min(CHUNK as u64) as usize);
                let n = self.base.read(&mut out[..limit])?;
                if n == 0 {
                    return Err(invalid("Delta base ended early"));
                }
                self.copy -= n as u64;
                self.copy_at += n as u64;
                self.pos += n as u64;
                return Ok(n);
            }
            if self.literal > 0 {
                let limit = out
                    .len()
                    .min(CHUNK)
                    .min(self.literal.min(self.size - self.pos).min(CHUNK as u64) as usize);
                let n = self.instructions.read(&mut out[..limit])?;
                if n == 0 {
                    return Err(invalid("Truncated delta literal"));
                }
                self.literal -= n as u64;
                self.pos += n as u64;
                return Ok(n);
            }
            let command = byte(&mut self.instructions)?;
            if command == 0 {
                return Err(invalid("Reserved delta opcode"));
            }
            if command & 128 == 0 {
                self.literal = command as u64;
                if self.pos + self.literal > self.size {
                    return Err(invalid("Delta exceeds target size"));
                }
            } else {
                let mut offset = 0;
                let mut length = 0;
                for i in 0..4 {
                    if command & (1 << i) != 0 {
                        offset |= (byte(&mut self.instructions)? as u64) << (i * 8)
                    }
                }
                for i in 0..3 {
                    if command & (1 << (i + 4)) != 0 {
                        length |= (byte(&mut self.instructions)? as u64) << (i * 8)
                    }
                }
                if length == 0 {
                    length = 0x10000
                }
                if offset + length > self.base.source.size || self.pos + length > self.size {
                    return Err(invalid("Delta copy exceeds object bounds"));
                }
                self.copy_at = offset;
                self.copy = length;
            }
        }
    }
}
#[derive(Clone)]
enum Base {
    Offset(u64),
    Oid(ObjectId),
}
#[derive(Clone)]
struct Entry {
    kind: u8,
    size: u64,
    body: Location,
    base: Option<Base>,
}
struct Store {
    io: Arc<dyn Io>,
    files: BTreeMap<String, FileMeta>,
    sources: RefCell<HashMap<ObjectId, Arc<Source>>>,
    new: RefCell<HashSet<ObjectId>>,
    indices: RefCell<VecDeque<(String, Arc<crate::pack_index::PackIndex>)>>,
    expanded:RefCell<HashMap<Location,Arc<Source>>>,
}
impl Store {
    fn new(io: Arc<dyn Io>, files: BTreeMap<String, FileMeta>) -> Self {
        Self {
            io,
            files,
            sources: RefCell::new(HashMap::new()),
            new: RefCell::new(HashSet::new()),
            indices: RefCell::new(VecDeque::new()),
            expanded:RefCell::new(HashMap::new()),
        }
    }
    fn raw(&self, key: &str) -> Result<Location> {
        Ok(Location::source(
            key,
            self.files
                .get(key)
                .ok_or_else(|| invalid(format!("Missing file: {key}")))?
                .size,
        ))
    }
    fn metadata(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let Some(meta) = self.files.get(key) else {
            return Ok(None);
        };
        check(
            meta.size <= META,
            "Git metadata exceeds the 32 MiB metadata budget",
        )?;
        let mut bytes = Vec::new();
        FileReader::new(self.io.clone(), Location::source(key, meta.size))
            .read_to_end(&mut bytes)?;
        Ok(Some(bytes))
    }
    fn text(&self, key: &str) -> Result<String> {
        Ok(self
            .metadata(key)?
            .map(|b| String::from_utf8_lossy(&b).trim().to_owned())
            .unwrap_or_default())
    }
    fn index(&self, key: &str) -> Result<Arc<crate::pack_index::PackIndex>> {
        if let Some((_, idx)) = self.indices.borrow().iter().find(|(k, _)| k == key) {
            return Ok(idx.clone());
        }
        let bytes = self
            .metadata(key)?
            .ok_or_else(|| invalid("Git pack index missing"))?;
        let idx = Arc::new(crate::pack_index::parse_pack_index_bytes(
            std::path::Path::new(key),
            bytes,
            true,
        )?);
        check(
            idx.hash_bytes() == 20,
            "Browser Git supports SHA-1 repositories",
        )?;
        let mut cache = self.indices.borrow_mut();
        if cache.len() >= 2 {
            cache.pop_front();
        }
        cache.push_back((key.into(), idx.clone()));
        Ok(idx)
    }
    fn original(&self, id: &ObjectId) -> Result<bool> {
        let hex = id.to_hex();
        if self
            .files
            .contains_key(&format!(".git/objects/{}/{}", &hex[..2], &hex[2..]))
        {
            return Ok(true);
        }
        for key in self
            .files
            .keys()
            .filter(|k| k.starts_with(".git/objects/pack/") && k.ends_with(".idx"))
        {
            if self.index(key)?.contains(id)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
    fn source(&self, id: ObjectId) -> Result<Arc<Source>> {
        self.source_depth(id, 0)
    }
    fn source_depth(&self, id: ObjectId, depth: usize) -> Result<Arc<Source>> {
        check(depth < 64, "Git delta chain exceeds 64 levels")?;
        if let Some(source) = self.sources.borrow().get(&id) {
            return Ok(source.clone());
        }
        let hex = id.to_hex();
        let path = format!(".git/objects/{}/{}", &hex[..2], &hex[2..]);
        if let Some(meta) = self.files.get(&path) {
            let loc = Location::source(&path, meta.size);
            let mut reader =
                flate2::read::ZlibDecoder::new(FileReader::new(self.io.clone(), loc.clone()));
            let mut header = Vec::new();
            for _ in 0..128 {
                let b = byte(&mut reader)?;
                header.push(b);
                if b == 0 {
                    break;
                }
            }
            check(header.last() == Some(&0), "Invalid loose object header")?;
            let text = std::str::from_utf8(&header[..header.len() - 1])?;
            let (name, size) = text
                .split_once(' ')
                .ok_or_else(|| invalid("Invalid loose object header"))?;
            let kind = match name {
                "blob" => ObjectKind::Blob,
                "tree" => ObjectKind::Tree,
                "commit" => ObjectKind::Commit,
                "tag" => ObjectKind::Tag,
                _ => return Err(invalid("Invalid loose object type").into()),
            };
            return Ok(Arc::new(Source {
                io: self.io.clone(),
                kind,
                size: size.parse()?,
                body: Body::Zlib(loc, header.len() as u64),
            }));
        }
        for key in self
            .files
            .keys()
            .filter(|k| k.starts_with(".git/objects/pack/") && k.ends_with(".idx"))
        {
            let idx = self.index(key)?;
            if let Some(offset) = idx.find_offset(&id)? {
                let pack = key.trim_end_matches(".idx").to_owned() + ".pack";
                let len = self
                    .files
                    .get(&pack)
                    .ok_or_else(|| invalid("Pack data missing"))?
                    .size;
                let end = idx.next_pack_offset_after(
                    offset,
                    len.checked_sub(20)
                        .ok_or_else(|| invalid("Truncated pack"))?,
                )?;
                let entry = self.entry(&pack, 0, offset, end)?;
                return self.resolve_entry(&pack, 0, &entry, Some(&idx), None, depth + 1);
            }
        }
        Err(invalid(format!("Git object {} is unavailable", id.to_hex())).into())
    }
    fn entry(&self, key: &str, start: u64, offset: u64, end: u64) -> Result<Entry> {
        check(end > offset, "Invalid packed object range")?;
        let mut reader = FileReader::new(
            self.io.clone(),
            Location {
                key: Some(key.into()),
                offset: start + offset,
                length: end - offset,
            },
        );
        let (code, size) = pack_header(&mut reader)?;
        let base = match code {
            6 => Some(Base::Offset(ofs_base(&mut reader, offset)?)),
            7 => {
                let mut id = [0; 20];
                reader.read_exact(&mut id)?;
                Some(Base::Oid(ObjectId::from_bytes(&id)?))
            }
            _ => {
                kind(code)?;
                None
            }
        };
        Ok(Entry {
            kind: code,
            size,
            body: Location {
                key: Some(key.into()),
                offset: start + offset + reader.pos,
                length: end - offset - reader.pos,
            },
            base,
        })
    }
    // Delta instructions seek backwards frequently. Expand their bases once to
    // the disk journal so each copy is a direct bounded read, not recursive
    // decompression of the entire earlier history on every seek.
    fn expand(&self,source:Arc<Source>)->Result<Arc<Source>>{
        let location=match &source.body{Body::Raw(_)=>return Ok(source),Body::Zlib(loc,_)=>loc.clone(),Body::Delta{instructions,..}=>instructions.clone()};
        if let Some(raw)=self.expanded.borrow().get(&location){return Ok(raw.clone());}
        let mut journal=Journal::new(self.io.clone(),false)?;let mut reader=source.open()?;let mut buffer=[0;CHUNK];let mut size=0;
        loop{let count=reader.read(&mut buffer)?;if count==0{break;}size+=count as u64;check(size<=source.size,"Delta output exceeds declared size")?;journal.write_all(&buffer[..count])?;}
        check(size==source.size,"Truncated delta output")?;let range=journal.finish()?;
        let raw=Arc::new(Source{io:self.io.clone(),kind:source.kind,size,body:Body::Raw(Location::journal(&range))});
        self.expanded.borrow_mut().insert(location,raw.clone());Ok(raw)
    }
    fn resolve_entry(
        &self,
        key: &str,
        start: u64,
        entry: &Entry,
        idx: Option<&crate::pack_index::PackIndex>,
        entries: Option<&BTreeMap<u64, Entry>>,
        depth: usize,
    ) -> Result<Arc<Source>> {
        check(depth < 64, "Git delta chain exceeds 64 levels")?;
        if let Some(raw)=self.expanded.borrow().get(&entry.body){return Ok(raw.clone());}
        let Some(base) = &entry.base else {
            return Ok(Arc::new(Source {
                io: self.io.clone(),
                kind: kind(entry.kind)?,
                size: entry.size,
                body: Body::Zlib(entry.body.clone(), 0),
            }));
        };
        let base = match base {
            Base::Oid(id) => self.source_depth(*id, depth + 1)?,
            Base::Offset(offset) => {
                let other = if let Some(entries) = entries {
                    entries
                        .get(offset)
                        .cloned()
                        .ok_or_else(|| invalid("Delta points outside pack"))?
                } else {
                    let idx = idx.ok_or_else(|| invalid("Missing pack index"))?;
                    let len = self
                        .files
                        .get(key)
                        .ok_or_else(|| invalid("Missing pack"))?
                        .size;
                    let end = idx.next_pack_offset_after(*offset, len - 20)?;
                    self.entry(key, start, *offset, end)?
                };
                self.resolve_entry(key, start, &other, idx, entries, depth + 1)?
            }
        };
        let base=self.expand(base)?;
        let mut reader =
            flate2::read::ZlibDecoder::new(FileReader::new(self.io.clone(), entry.body.clone()));
        let base_size = varint(&mut reader)?;
        let size = varint(&mut reader)?;
        check(base_size == base.size, "Delta base size mismatch")?;
        self.expand(Arc::new(Source {
            io: self.io.clone(),
            kind: base.kind,
            size,
            body: Body::Delta {base,instructions:entry.body.clone()},
        }))
    }
    fn read_small(&self, id: ObjectId) -> Result<(ObjectKind, Vec<u8>)> {
        let source = self.source(id)?;
        check(
            source.size <= META,
            "Git tree/commit exceeds metadata budget",
        )?;
        let mut bytes = Vec::new();
        source
            .open()?
            .take(source.size + 1)
            .read_to_end(&mut bytes)?;
        check(
            bytes.len() as u64 == source.size,
            "Git object size mismatch",
        )?;
        check(
            crate::hash::hash_object(HashAlgo::Sha1, source.kind, &bytes)? == id,
            "Git object checksum mismatch",
        )?;
        Ok((source.kind, bytes))
    }
    fn put(&self, kind: ObjectKind, bytes: &[u8]) -> Result<ObjectId> {
        let id = crate::hash::hash_object(HashAlgo::Sha1, kind, bytes)?;
        if !self.original(&id)? {
            let range = stage(self.io.clone(), bytes)?;
            self.sources.borrow_mut().insert(
                id,
                Arc::new(Source {
                    io: self.io.clone(),
                    kind,
                    size: bytes.len() as u64,
                    body: Body::Raw(Location::journal(&range)),
                }),
            );
            self.new.borrow_mut().insert(id);
        }
        Ok(id)
    }
    fn blob(&self, path: &str) -> Result<ObjectId> {
        let loc = self.raw(path)?;
        let source = Arc::new(Source {
            io: self.io.clone(),
            kind: ObjectKind::Blob,
            size: loc.length,
            body: Body::Raw(loc),
        });
        let id = hash_source(&source)?;
        if !self.original(&id)? {
            self.new.borrow_mut().insert(id);
            self.sources.borrow_mut().insert(id, source);
        }
        Ok(id)
    }
    fn walk(&self, tip: ObjectId) -> Result<HashSet<ObjectId>> {
        let mut pending = vec![tip];
        let mut seen = HashSet::new();
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                continue;
            }
            check(
                seen.len() <= OBJECTS,
                "Git history exceeds the metadata budget",
            )?;
            let source = self.source(id)?;
            match source.kind {
                ObjectKind::Commit => {
                    let (_, bytes) = self.read_small(id)?;
                    let c = parse_commit(&bytes)?;
                    pending.push(c.tree);
                    pending.extend(c.parents);
                }
                ObjectKind::Tree => {
                    let (_, bytes) = self.read_small(id)?;
                    for e in parse_tree_with_oid_len(&bytes, 20)? {
                        if e.mode != 0o160000 {
                            pending.push(e.oid)
                        }
                    }
                }
                ObjectKind::Tag => {
                    let (_, bytes) = self.read_small(id)?;
                    pending.push(crate::objects::parse_tag(&bytes)?.object);
                }
                _ => {}
            }
        }
        Ok(seen)
    }
    fn tree(&self, paths: &BTreeMap<String, (ObjectId, u32)>, prefix: &str) -> Result<ObjectId> {
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
                    })
                }
            }
        }
        for dir in dirs {
            let oid = self.tree(paths, &format!("{prefix}{dir}/"))?;
            entries.push(TreeEntry {
                mode: 0o40000,
                name: dir.into_bytes(),
                oid,
            });
        }
        let entries = crate::objects::write::canonical_tree_entries(entries, HashAlgo::Sha1)?;
        self.put(ObjectKind::Tree, &serialize_tree(&entries))
    }
}
fn hash_source(source: &Arc<Source>) -> Result<ObjectId> {
    let mut hash = crate::hash::ObjectHasher::for_object(HashAlgo::Sha1, source.kind, source.size);
    let mut reader = source.open()?;
    let mut buf = [0; CHUNK];
    let mut n = 0;
    loop {
        let count = reader.read(&mut buf)?;
        if count == 0 {
            break;
        }
        n += count as u64;
        check(n <= source.size, "Git object exceeds declared size")?;
        hash.update(&buf[..count]);
    }
    check(n == source.size, "Git object is truncated")?;
    Ok(hash.finalize()?)
}
fn checksum(io: Arc<dyn Io>, loc: Location) -> Result<String> {
    let mut hash = Sha256::new();
    let mut reader = FileReader::new(io, loc);
    let mut buffer = [0; CHUNK];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

impl Store {
    fn scan_pack(&self, key: &str, start: u64) -> Result<()> {
        let size = self
            .files
            .get(key)
            .ok_or_else(|| invalid("Bundle file missing"))?
            .size;
        check(size >= start + 32, "Truncated Git pack")?;
        let mut reader = io::BufReader::with_capacity(
            CHUNK,
            FileReader::new(
                self.io.clone(),
                Location {
                    key: Some(key.into()),
                    offset: start,
                    length: size - start,
                },
            ),
        );
        let mut header = [0; 12];
        reader.read_exact(&mut header)?;
        check(
            &header[..4] == b"PACK"
                && matches!(u32::from_be_bytes(header[4..8].try_into()?), 2 | 3),
            "Invalid Git pack header",
        )?;
        let count = u32::from_be_bytes(header[8..].try_into()?) as usize;
        check(count <= OBJECTS, "Pack exceeds the object metadata budget")?;
        let mut entries = BTreeMap::new();
        let mut unresolved = Vec::new();
        for _ in 0..count {
            let offset = reader.stream_position()?;
            let (code, declared) = pack_header(&mut reader)?;
            let base = match code {
                6 => Some(Base::Offset(ofs_base(&mut reader, offset)?)),
                7 => {
                    let mut id = [0; 20];
                    reader.read_exact(&mut id)?;
                    Some(Base::Oid(ObjectId::from_bytes(&id)?))
                }
                _ => {
                    kind(code)?;
                    None
                }
            };
            let body_start = reader.stream_position()?;
            let mut decoder = flate2::bufread::ZlibDecoder::new(&mut reader);
            let mut hash = if base.is_none() {
                Some(crate::hash::ObjectHasher::for_object(
                    HashAlgo::Sha1,
                    kind(code)?,
                    declared,
                ))
            } else {
                None
            };
            let mut produced = 0;
            let mut buffer = [0; CHUNK];
            loop {
                let n = decoder.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                produced += n as u64;
                check(produced <= declared, "Packed object exceeds declared size")?;
                if let Some(hash) = &mut hash {
                    hash.update(&buffer[..n]);
                }
            }
            check(produced == declared, "Packed object size mismatch")?;
            drop(decoder);
            let end = reader.stream_position()?;
            let entry = Entry {
                kind: code,
                size: declared,
                body: Location {
                    key: Some(key.into()),
                    offset: start + body_start,
                    length: end - body_start,
                },
                base,
            };
            if let Some(hash) = hash {
                let id = hash.finalize()?;
                let source = Arc::new(Source {
                    io: self.io.clone(),
                    kind: kind(code)?,
                    size: declared,
                    body: Body::Zlib(entry.body.clone(), 0),
                });
                self.sources.borrow_mut().entry(id).or_insert(source);
            } else {
                unresolved.push(offset);
            }
            entries.insert(offset, entry);
            check(
                self.sources.borrow().len() <= OBJECTS,
                "Git history exceeds the metadata budget",
            )?;
        }
        let body_len = reader.stream_position()?;
        let mut trailer = [0; 20];
        reader.read_exact(&mut trailer)?;
        let mut extra = [0];
        check(
            reader.read(&mut extra)? == 0,
            "Unexpected bytes after Git pack trailer",
        )?;
        let mut verify = crate::hash::ChecksumHasher::new(HashAlgo::Sha1);
        let mut file = FileReader::new(
            self.io.clone(),
            Location {
                key: Some(key.into()),
                offset: start,
                length: body_len,
            },
        );
        let mut buffer = [0; CHUNK];
        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            verify.update(&buffer[..n]);
        }
        check(verify.finalize() == trailer, "Git pack checksum mismatch")?;
        while !unresolved.is_empty() {
            let before = unresolved.len();
            let mut retry = Vec::new();
            for offset in unresolved {
                let entry = entries
                    .get(&offset)
                    .ok_or_else(|| invalid("Missing delta entry"))?;
                match self.resolve_entry(key, start, entry, None, Some(&entries), 0) {
                    Ok(source) => {
                        let id = hash_source(&source)?;
                        self.sources.borrow_mut().entry(id).or_insert(source);
                    }
                    Err(error) if error.to_string().contains("is unavailable") => {
                        retry.push(offset)
                    }
                    Err(error) => return Err(error),
                }
            }
            check(
                retry.len() < before,
                "Git bundle has unresolved delta bases",
            )?;
            unresolved = retry;
        }
        Ok(())
    }
    fn import_bundles(
        &self,
        selected: &[String],
        cached: &[String],
        expected: Option<&str>,
    ) -> Result<(ObjectId, Vec<BundleRange>)> {
        let mut headers = BTreeMap::new();
        for key in selected.iter().chain(cached) {
            if headers.contains_key(key) {
                continue;
            }
            let mut loc = self.raw(key)?;
            loc.length = loc.length.min(1024 * 1024);
            let mut reader =
                io::BufReader::with_capacity(CHUNK, FileReader::new(self.io.clone(), loc));
            let header = crate::bundle::read_header(&mut reader)?;
            let start = reader.stream_position()?;
            check(start <= 1024 * 1024, "Git bundle header is too large")?;
            check(
                header.object_format == HashAlgo::Sha1,
                "Browser Git supports SHA-1 bundles",
            )?;
            headers.insert(key.clone(), (header, start));
        }
        fn load(
            store: &Store,
            key: &str,
            headers: &BTreeMap<String, (crate::bundle::BundleHeader, u64)>,
            loaded: &mut HashSet<String>,
            loading: &mut HashSet<String>,
        ) -> Result<()> {
            if loaded.contains(key) {
                return Ok(());
            }
            check(
                loading.insert(key.into()),
                "Git bundle prerequisites contain a cycle",
            )?;
            let (header, start) = headers
                .get(key)
                .ok_or_else(|| invalid("Git prerequisite bundle is missing"))?;
            for (base, _) in &header.prerequisites {
                if store.source(*base).is_err() {
                    let source = headers
                        .iter()
                        .find(|(other, (head, _))| {
                            other.as_str() != key && head.refs.iter().any(|(_, id)| id == base)
                        })
                        .map(|(k, _)| k)
                        .ok_or_else(|| {
                            invalid(format!("Missing prerequisite bundle for {}", base.to_hex()))
                        })?;
                    load(store, source, headers, loaded, loading)?;
                }
            }
            store.scan_pack(key, *start)?;
            loading.remove(key);
            loaded.insert(key.into());
            Ok(())
        }
        let mut loaded = HashSet::new();
        let mut loading = HashSet::new();
        let mut ranges = Vec::new();
        for (index, key) in selected.iter().enumerate() {
            load(self, key, &headers, &mut loaded, &mut loading)?;
            let (h, _) = &headers[key];
            ranges.push(BundleRange {
                index,
                bases: h.prerequisites.iter().map(|(id, _)| id.to_hex()).collect(),
                tips: h.refs.iter().map(|(_, id)| id.to_hex()).collect(),
            });
        }
        let key = selected
            .last()
            .ok_or_else(|| invalid("Choose a Git bundle"))?;
        let (header, _) = &headers[key];
        let tip = header
            .refs
            .iter()
            .find(|(name, _)| name == "HEAD")
            .or_else(|| header.refs.first())
            .map(|(_, id)| *id)
            .ok_or_else(|| invalid("Bundle has no Git reference"))?;
        if let Some(expected) = expected {
            check(
                tip.to_hex() == expected,
                "Bundle does not match the announced tip",
            )?;
        }
        check(
            self.source(tip)?.kind == ObjectKind::Commit,
            "Bundle tip is not a commit",
        )?;
        self.walk(tip)?;
        Ok((tip, ranges))
    }
    fn loose(&self, id: ObjectId) -> Result<Range> {
        let source = self.source(id)?;
        let mut writer = flate2::write::ZlibEncoder::new(
            Journal::new(self.io.clone(), false)?,
            flate2::Compression::default(),
        );
        write!(writer, "{} {}\0", source.kind.as_str(), source.size)?;
        copy_verified(&source, id, &mut writer)?;
        Ok(writer.finish()?.finish()?)
    }
    fn emit(&self, ids: &HashSet<ObjectId>) -> Result<BTreeMap<String, Range>> {
        let mut writes = BTreeMap::new();
        for id in ids {
            if self.original(id)? {
                continue;
            }
            let hex = id.to_hex();
            writes.insert(
                format!(".git/objects/{}/{}", &hex[..2], &hex[2..]),
                self.loose(*id)?,
            );
        }
        Ok(writes)
    }
    fn snapshot(&self, tip: ObjectId, extract:bool) -> Result<(BTreeMap<String, Range>, Vec<String>)> {
        let (_, bytes) = self.read_small(tip)?;
        let root = parse_commit(&bytes)?.tree;
        let mut files = BTreeMap::new();
        let mut pending = vec![(String::new(), root, 0)];
        while let Some((prefix, id, depth)) = pending.pop() {
            check(depth <= 32, "KiCad tree exceeds maximum directory depth")?;
            let (kind, bytes) = self.read_small(id)?;
            check(kind == ObjectKind::Tree, "Expected a Git tree")?;
            for e in parse_tree_with_oid_len(&bytes, 20)? {
                let Ok(name) = String::from_utf8(e.name) else {
                    continue;
                };
                if name.is_empty() || name.starts_with('.') || name.contains(['/', '\\', '\0']) {
                    continue;
                }
                let path = format!("{prefix}{name}");
                if e.mode == 0o40000 {
                    if !name.ends_with("-backups") {
                        pending.push((format!("{path}/"), e.oid, depth + 1));
                    }
                } else if matches!(e.mode, 0o100644 | 0o100755)
                    && name.rsplit_once('.').is_some_and(|(_, ext)| {
                        matches!(ext, "kicad_pro" | "kicad_sch" | "kicad_pcb" | "kicad_dru")
                    })
                {
                    check(files.len() < 512, "Too many KiCad files")?;
                    if extract {
                        let source = self.source(e.oid)?;
                        let mut writer = Journal::new(self.io.clone(), false)?;
                        copy_verified(&source, e.oid, &mut writer)?;
                        files.insert(path, writer.finish()?);
                    }else{files.insert(path,Range{offset:0,length:0,sha256:None});}
                }
            }
        }
        let choices = files
            .keys()
            .filter(|p| p.ends_with(".kicad_pro"))
            .cloned()
            .collect::<Vec<_>>();
        check(
            !choices.is_empty(),
            "Commit contains no KiCad project files",
        )?;
        Ok((files, choices))
    }
    fn bundle(
        &self,
        tip: ObjectId,
        reference: &str,
        base: Option<ObjectId>,
        all: &HashSet<ObjectId>,
    ) -> Result<Range> {
        let excluded = if let Some(base) = base {
            self.walk(base)?
        } else {
            HashSet::new()
        };
        let mut ids = all.difference(&excluded).copied().collect::<Vec<_>>();
        ids.sort();
        let mut writer = Journal::new(self.io.clone(), true)?;
        writer.write_all(b"# v2 git bundle\n")?;
        if let Some(base) = base {
            writeln!(writer, "-{} previous tip", base.to_hex())?;
        }
        writeln!(writer, "{} {reference}\n", tip.to_hex())?;
        let mut pack_hash = crate::hash::ChecksumHasher::new(HashAlgo::Sha1);
        {
            let mut pack = PackWriter {
                inner: &mut writer,
                hash: &mut pack_hash,
            };
            pack.write_all(b"PACK")?;
            pack.write_all(&2u32.to_be_bytes())?;
            pack.write_all(&(u32::try_from(ids.len())?).to_be_bytes())?;
            for id in ids {
                let source = self.source(id)?;
                let mut header = Vec::new();
                encode_header(&mut header, source.kind, source.size);
                pack.write_all(&header)?;
                let mut encoder =
                    flate2::write::ZlibEncoder::new(&mut pack, flate2::Compression::default());
                copy_verified(&source, id, &mut encoder)?;
                encoder.finish()?;
            }
        }
        writer.write_all(&pack_hash.finalize())?;
        Ok(writer.finish()?)
    }
}
struct PackWriter<'a> {
    inner: &'a mut Journal,
    hash: &'a mut crate::hash::ChecksumHasher,
}
impl Write for PackWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.inner.write_all(bytes)?;
        self.hash.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
fn encode_header(out: &mut Vec<u8>, kind: ObjectKind, mut size: u64) {
    let code = match kind {
        ObjectKind::Commit => 1,
        ObjectKind::Tree => 2,
        ObjectKind::Blob => 3,
        ObjectKind::Tag => 4,
    };
    let first = (code << 4) | (size & 15) as u8;
    size >>= 4;
    out.push(first | if size > 0 { 128 } else { 0 });
    while size > 0 {
        let b = (size & 127) as u8;
        size >>= 7;
        out.push(b | if size > 0 { 128 } else { 0 });
    }
}
fn copy_verified(source: &Arc<Source>, id: ObjectId, out: &mut dyn Write) -> Result<()> {
    let mut hash = crate::hash::ObjectHasher::for_object(HashAlgo::Sha1, source.kind, source.size);
    let mut reader = source.open()?;
    let mut count = 0;
    let mut buffer = [0; CHUNK];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        count += n as u64;
        check(count <= source.size, "Object exceeds declared size")?;
        hash.update(&buffer[..n]);
        out.write_all(&buffer[..n])?;
    }
    check(
        count == source.size && hash.finalize()? == id,
        "Git object checksum mismatch",
    )?;
    Ok(())
}

/// Execute local Git or bundle extraction using bounded payload buffers.
fn recent_history(store:&Store, tip:ObjectId,limit:usize)->Result<Vec<serde_json::Value>> {
    let mut queue=VecDeque::from([tip]);let mut seen=HashSet::new();let mut history=Vec::new();
    while let Some(id)=queue.pop_front(){
        if !seen.insert(id){continue;}
        let (_,bytes)=store.read_small(id)?;let commit=parse_commit(&bytes)?;
        history.push(serde_json::json!({"hash":id.to_hex(),"author":commit.author,"message":commit.message.lines().next().unwrap_or("").chars().take(500).collect::<String>(),"parents":commit.parents.iter().map(|p|p.to_hex()).collect::<Vec<_>>() }));
        if history.len()>=limit{break;}
        queue.extend(commit.parents);
    }
    Ok(history)
}

// KiCad lockfiles are process-local, and Chromium rejects their temporary
// filenames on native filesystem handles. Keep their objects/history and mark
// them skip-worktree rather than accidentally staging their deletion.
fn omitted_kicad_lock(path:&str)->bool{
    let name=path.rsplit('/').next().unwrap_or(path);
    name.starts_with('~')&&[".kicad_pro.lck",".kicad_sch.lck",".kicad_pcb.lck"].iter().any(|suffix|name.ends_with(suffix))
}
fn checkout_paths(store:&Store,tip:ObjectId)->Result<BTreeMap<String,(ObjectId,u32)>>{
    let mut paths=BTreeMap::new();
    let root=parse_commit(&store.read_small(tip)?.1)?.tree;
    let mut pending=vec![(String::new(),root,0)];
    while let Some((prefix,id,depth))=pending.pop(){
        check(depth<32,"Repository tree nesting exceeds 32 levels")?;
        for entry in parse_tree_with_oid_len(&store.read_small(id)?.1,20)?{
            let name=String::from_utf8(entry.name)?;
            check(!name.is_empty()&&name!="."&&name!=".."&&!name.eq_ignore_ascii_case(".git")&&!name.contains(['/', '\\','\0',':']),"Unsafe repository path")?;
            let path=format!("{prefix}{name}");
            if entry.mode==0o40000{pending.push((format!("{path}/"),entry.oid,depth+1));}else{
                check(matches!(entry.mode,0o100644|0o100755),"Browser checkout supports regular files only")?;
                paths.insert(path,(entry.oid,entry.mode));
            }
        }
    }
    Ok(paths)
}
fn checkout_plan(store:&Store,tip:ObjectId,parent:Option<ObjectId>,reference:&str)->Result<Output>{
    if parent==Some(tip){return Ok(Output{initialized:true,head:Some(tip.to_hex()),reference:reference.into(),selected_tip:Some(tip.to_hex()),choices:store.snapshot(tip,false)?.1,..Default::default()});}
    let all=store.walk(tip)?;
    if let Some(parent)=parent{check(all.contains(&parent),"Repository has diverged; automatic checkout paused")?;}
    let paths=checkout_paths(store,tip)?;
    let recovering=parent.is_none()&&store.files.contains_key(".git/HEAD");
    if recovering {check(store.files.keys().filter(|path|!path.starts_with(".git/")).all(|path|paths.contains_key(path)||path.starts_with('\0')),"Interrupted checkout contains unrelated files; choose a new empty folder")?;}
    let old=if let Some(bytes)=store.metadata(".git/index")?{crate::index::Index::parse(&bytes)?}else{crate::index::Index::new()};
    let tracked=old.entries().iter().map(|e|(String::from_utf8_lossy(&e.path).into_owned(),e.oid)).collect::<BTreeMap<_,_>>();
    let mut output=Output{initialized:true,head:parent.map(|id|id.to_hex()),reference:reference.into(),selected_tip:Some(tip.to_hex()),..Default::default()};
    output.writes=store.emit(&all)?;
    let mut index=crate::index::Index::new();
    for (path,(oid,mode)) in &paths{
        let source=store.source(*oid)?;
        if !omitted_kicad_lock(path)&&tracked.get(path)!=Some(oid){
            if recovering&&store.files.contains_key(path){
                let existing=Arc::new(Source{io:store.io.clone(),kind:ObjectKind::Blob,size:store.files[path].size,body:Body::Raw(store.raw(path)?)});
                check(hash_source(&existing)?==*oid,"Interrupted checkout contains modified files; choose a new empty folder")?;
            }else{
            check(!store.files.contains_key(path)||tracked.contains_key(path),"Checkout would overwrite an untracked file")?;
            let mut writer=Journal::new(store.io.clone(),false)?;copy_verified(&source,*oid,&mut writer)?;output.writes.insert(path.clone(),writer.finish()?);
            }
        }
        let mut entry=crate::index::IndexEntry{ctime_sec:0,ctime_nsec:0,mtime_sec:0,mtime_nsec:0,dev:0,ino:0,mode:*mode,uid:0,gid:0,size:crate::index::munge_index_size(source.size),oid:*oid,flags:0,flags_extended:None,path:path.as_bytes().to_vec(),base_index_pos:0};
        entry.set_skip_worktree(omitted_kicad_lock(path));index.entries_mut().push(entry);
    }
    output.removes=tracked.keys().filter(|path|!omitted_kicad_lock(path)&&!paths.contains_key(*path)).cloned().collect();
    index.sort();let mut encoded=Vec::new();index.serialize_into(&mut encoded)?;
    let mut hash=crate::hash::ChecksumHasher::new(HashAlgo::Sha1);hash.update(&encoded);encoded.extend(hash.finalize());
    output.writes.insert(".git/index".into(),stage(store.io.clone(),&encoded)?);
    output.writes.insert(format!(".git/{reference}"),stage(store.io.clone(),format!("{}\n",tip.to_hex()).as_bytes())?);
    if parent.is_none(){
        output.writes.insert(".git/HEAD".into(),stage(store.io.clone(),format!("ref: {reference}\n").as_bytes())?);
        output.writes.insert(".git/config".into(),stage(store.io.clone(),b"[core]\n repositoryformatversion = 0\n bare = false\n filemode = false\n")?);
    }
    output.choices=paths.keys().filter(|path|path.ends_with(".kicad_pro")).cloned().collect();
    check(!output.choices.is_empty(),"Repository has no KiCad project file")?;Ok(output)
}
/// Execute a bounded repository job, returning staged disk ranges.
pub fn execute(io: Arc<dyn Io>, input: Input) -> Result<Output> {
    check(
        input.files.len() <= 100_000,
        "Repository exceeds the file metadata budget",
    )?;
    for path in input.files.keys() {
        if input.imports.contains(path) || input.cached.contains(path) {
            continue;
        }
        check(
            !path.starts_with('/')
                && !path.contains('\\')
                && !path
                    .split('/')
                    .any(|p| p.is_empty() || p == ".." || p == "."),
            "Unsafe repository path",
        )?;
    }
    let store = Store::new(io.clone(), input.files);
    if input.operation == "hash" {
        let key = input
            .imports
            .first()
            .ok_or_else(|| invalid("Missing checksum file"))?;
        return Ok(Output {
            checksum: Some(checksum(io, store.raw(key)?)?),
            ..Default::default()
        });
    }
    let mut output = Output::default();
    let imported = if !input.imports.is_empty() {
        if let Some(expected) = &input.expected_sha256 {
            let key = input
                .imports
                .last()
                .ok_or_else(|| invalid("Missing bundle"))?;
            check(
                checksum(io.clone(), store.raw(key)?)? == *expected,
                "Bundle checksum mismatch",
            )?;
        }
        let (tip, ranges) =
            store.import_bundles(&input.imports, &input.cached, input.tip.as_deref())?;
        output.ranges = ranges;
        Some(tip)
    } else {
        None
    };
    if input.operation=="bundle_log"{
        let tip=imported.ok_or_else(||invalid("No received bundle"))?;
        output.head=Some(tip.to_hex());output.history=recent_history(&store,tip,input.history_limit.max(1))?;return Ok(output);
    }
    if matches!(input.operation.as_str(), "extract" | "bundle" | "unpack") {
        let tip = imported.ok_or_else(|| invalid("Missing received bundle"))?;
        output.selected_tip = Some(tip.to_hex());
        if input.operation == "bundle" {
            let all = store.walk(tip)?;
            output.bundle = Some(store.bundle(tip, "refs/heads/main", None, &all)?);
        } else {
            if input.operation=="unpack" {output.writes=store.emit(&store.walk(tip)?)?;}
            (output.snapshot, output.choices) = store.snapshot(tip,input.operation!="unpack")?;
        }
        return Ok(output);
    }
    check(
        !store
            .files
            .keys()
            .any(|p| p.starts_with(".git/") && p.ends_with(".lock")),
        "Repository is locked by another Git operation",
    )?;
    check(
        !store.files.contains_key(".git") && !store.files.contains_key(".git/commondir"),
        "Linked worktrees require local Git",
    )?;
    let initialized = store.files.contains_key(".git/HEAD");
    output.initialized = initialized;
    if input.operation=="checkout" && !initialized{
        check(store.files.keys().all(|path|input.imports.contains(path)||input.cached.contains(path)),"Choose an empty folder or a matching Git repository")?;
        return checkout_plan(&store,imported.ok_or_else(||invalid("No received Git bundle"))?,None,"refs/heads/main");
    }
    if input.operation == "init" {
        check(!initialized, "Repository already exists")?;
        output.reference = "refs/heads/main".into();
        output.initialized = true;
        output.writes.insert(
            ".git/HEAD".into(),
            stage(io.clone(), b"ref: refs/heads/main\n")?,
        );
        output.writes.insert(
            ".git/config".into(),
            stage(
                io.clone(),
                b"[core]\n\trepositoryformatversion = 0\n\tbare = false\n\tfilemode = false\n",
            )?,
        );
        let mut ignore = store.text(".gitignore")?;
        for line in super::INITIAL_IGNORE_RULES.lines() {
            if !ignore.lines().any(|p| p.trim() == line) {
                if !ignore.is_empty() {
                    ignore.push('\n')
                }
                ignore.push_str(line)
            }
        }
        ignore.push('\n');
        output
            .writes
            .insert(".gitignore".into(), stage(io, ignore.as_bytes())?);
        return Ok(output);
    }
    check(initialized, "Initialize the repository first")?;
    check(
        !store.files.contains_key(".git/MERGE_HEAD")
            && !store.files.contains_key(".git/rebase-merge/head-name")
            && !store.files.contains_key(".git/rebase-apply/head-name"),
        "Finish the merge or rebase with local Git first",
    )?;
    let config = store
        .text(".git/config")?
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>();
    check(
        !config.contains("objectformat=sha256")
            && !config.contains("refstorage=reftable")
            && !store.files.contains_key(".git/reftable/tables.list"),
        "Browser Git currently supports SHA-1 repositories with file refs",
    )?;
    let head = store.text(".git/HEAD")?;
    let reference = head
        .strip_prefix("ref: ")
        .ok_or_else(|| invalid("Check out a branch with local Git first"))?
        .to_owned();
    check(
        reference.starts_with("refs/heads/")
            && !reference.contains("..")
            && !reference.contains('\\'),
        "Invalid Git reference",
    )?;
    let mut id = store.text(&format!(".git/{reference}"))?;
    if id.is_empty() {
        for line in store.text(".git/packed-refs")?.lines() {
            if let Some((oid, name)) = line.split_once(' ') {
                if name == reference {
                    id = oid.into()
                }
            }
        }
    }
    let parent = if id.is_empty() {
        None
    } else {
        Some(ObjectId::from_hex(&id)?)
    };
    output.head = parent.map(|p| p.to_hex());
    output.reference = reference.clone();
    if input.operation=="log"{if let Some(tip)=parent{output.history=recent_history(&store,tip,input.history_limit.max(1))?;}return Ok(output);}
    if input.operation=="reachable"{
        if let Some(tip)=parent{
            let mut remaining=input.known_tips.into_iter().collect::<HashSet<_>>();
            let mut seen=HashSet::new();let mut queue=VecDeque::from([tip]);
            while let Some(id)=queue.pop_front(){
                if !seen.insert(id){continue;}
                if remaining.remove(&id.to_hex()){output.ancestors.push(id.to_hex());}
                if remaining.is_empty(){break;}
                queue.extend(parse_commit(&store.read_small(id)?.1)?.parents);
            }
        }
        return Ok(output);
    }
    if input.operation == "import" {
        check(imported.is_some(), "No received Git history is available")?;
        let ids = store
            .sources
            .borrow()
            .keys()
            .copied()
            .collect::<HashSet<_>>();
        output.writes = store.emit(&ids)?;
        for range in &output.ranges {
            for tip in &range.tips {
                output.writes.insert(
                    format!(".git/refs/vinventory/bundles/{tip}"),
                    stage(io.clone(), format!("{tip}\n").as_bytes())?,
                );
            }
        }
        return Ok(output);
    }
    // Attaching an existing checkout publishes its current commit without
    // staging working changes or creating a new commit.
    if input.operation == "share" {
        let Some(tip)=parent else { return Ok(output); };
        let all=store.walk(tip)?;
        output.bundle=Some(store.bundle(tip,&reference,None,&all)?);
        output.tip=Some(tip.to_hex());
        output.ancestors=input.known_tips.into_iter().filter(|id|ObjectId::from_hex(id).is_ok_and(|id|all.contains(&id)&&id!=tip)).collect();
        (output.snapshot,output.choices)=store.snapshot(tip,true)?;
        output.selected_tip=Some(tip.to_hex());
        return Ok(output);
    }
    let old_index = if let Some(bytes) = store.metadata(".git/index")? {
        crate::index::Index::parse(&bytes)?
    } else {
        crate::index::Index::new()
    };
    check(
        !old_index.has_unmerged_entries()
            && !old_index
                .entries()
                .iter()
                .any(|e| (e.skip_worktree()&&!omitted_kicad_lock(&String::from_utf8_lossy(&e.path))) || matches!(e.mode, 0o120000 | 0o160000)),
        "Use local Git for unmerged entries, sparse checkouts, symlinks or submodules",
    )?;
    let tracked = old_index
        .entries()
        .iter()
        .map(|e| (String::from_utf8_lossy(&e.path).into_owned(), e))
        .collect::<HashMap<_, _>>();
    let mut ignores = BTreeMap::new();
    for path in store.files.keys().filter(|p| {
        p.as_str() == ".gitignore"
            || p.ends_with("/.gitignore")
            || p.as_str() == ".git/info/exclude"
    }) {
        ignores.insert(path.clone(), store.metadata(path)?.unwrap_or_default());
    }
    let index_bytes = store.metadata(".git/index")?;
    let selected = super::select_files(
        index_bytes.as_deref(),
        ignores,
        store
            .files
            .keys()
            .filter(|p| !input.imports.contains(p) && !input.cached.contains(p))
            .cloned()
            .collect(),
        false,
    )?;
    for path in selected.iter().filter(|p| p.ends_with(".gitattributes")) {
        check(
            !store.text(path)?.contains("filter="),
            "Use local Git for content filters such as Git LFS",
        )?;
    }
    let mut paths = BTreeMap::new();
    let mut index = crate::index::Index::new();
    for path in selected {
        if path == ".git" || path.starts_with(".git/") {
            continue;
        }
        if tracked.get(&path).is_some_and(|entry|entry.skip_worktree()&&omitted_kicad_lock(&path)){continue;}
        let oid = store.blob(&path)?;
        let mode = tracked.get(&path).map_or(0o100644, |e| e.mode);
        let meta = &store.files[&path];
        if tracked.get(&path).is_none_or(|e| e.oid != oid) {
            output.changes.push(path.clone());
        }
        index.entries_mut().push(crate::index::IndexEntry {
            ctime_sec: 0,
            ctime_nsec: 0,
            mtime_sec: (meta.modified / 1000) as u32,
            mtime_nsec: ((meta.modified % 1000) * 1_000_000) as u32,
            dev: 0,
            ino: 0,
            mode,
            uid: 0,
            gid: 0,
            size: crate::index::munge_index_size(meta.size),
            oid,
            flags: 0,
            flags_extended: None,
            path: path.as_bytes().to_vec(),
            base_index_pos: 0,
        });
        paths.insert(path, (oid, mode));
    }
    for (path,entry) in &tracked {
        if entry.skip_worktree()&&omitted_kicad_lock(path){
            index.entries_mut().push((*entry).clone());paths.insert(path.clone(),(entry.oid,entry.mode));
        }
        if !paths.contains_key(path) {
            output.changes.push(path.clone())
        }
    }
    let root = store.tree(&paths, "")?;
    if let Some(parent) = parent {
        let (_, bytes) = store.read_small(parent)?;
        if parse_commit(&bytes)?.tree == root {
            output.changes.clear();
        }
    }
    output.change_details=output.changes.iter().map(|path|{
        let text=|id:Option<ObjectId>|->Option<String>{
            let Some(id)=id else{return Some(String::new())};
            if store.source(id).ok()?.size>128*1024{return None}
            let (_,bytes)=store.read_small(id).ok()?;if bytes.contains(&0){return None}
            String::from_utf8(bytes).ok().filter(|text|text.lines().take(5001).count()<=5000)
        };
        let lines=if input.operation=="status" {
            text(tracked.get(path).map(|entry|entry.oid)).zip(text(paths.get(path).map(|(id,_)|*id))).map(|(old,new)|{
                let counts=crate::diff::line_change_counts(&old,&new,&crate::diff::DiffOptions::default());(counts.insertions,counts.deletions)
            })
        }else{None};
        ChangeDetail{path:path.clone(),kind:if !paths.contains_key(path){"Removed"}else if !tracked.contains_key(path){"Added"}else{"Modified"},lines}
    }).collect();
    if input.operation=="checkout" {
        let tip=imported.ok_or_else(||invalid("No received Git bundle"))?;
        check(parent==Some(tip)||parent.map(|id|id.to_hex())==input.checkout_head,"Repository HEAD differs from the last applied commit")?;
        if parent.is_none(){
            check(input.recover_checkout&&!store.files.contains_key(".git/index"),"Repository has no committed branch; choose an empty folder or a matching repository")?;
            // A copy may have stopped before its branch/index was written and a
            // newer announcement may have arrived since. Recognize only an
            // ancestor whose existing working files are byte-for-byte identical.
            let existing=store.files.keys().filter(|path|!path.starts_with(".git/")&&!path.starts_with('\0')).map(|path|{
                let source=Arc::new(Source{io:store.io.clone(),kind:ObjectKind::Blob,size:store.files[path].size,body:Body::Raw(store.raw(path)?)});
                Ok((path.clone(),hash_source(&source)?))
            }).collect::<Result<BTreeMap<_,_>>>()?;
            let mut candidates=vec![tip];let mut seen=HashSet::new();
            while let Some(candidate)=candidates.pop(){
                if !seen.insert(candidate){continue;}
                let commit=parse_commit(&store.read_small(candidate)?.1)?;
                if store.original(&candidate)?{
                    let paths=checkout_paths(&store,candidate)?;
                    if existing.iter().all(|(path,oid)|paths.get(path).is_some_and(|(expected,_)|expected==oid)){
                        return checkout_plan(&store,candidate,None,&reference);
                    }
                }
                candidates.extend(commit.parents);
            }
            return Err(invalid("Interrupted checkout contains modified or unrelated files; choose a new empty folder").into());
        }
        if parent!=Some(tip){
            check(output.changes.is_empty()&&parent.is_some_and(|id|store.read_small(id).ok().and_then(|(_,bytes)|parse_commit(&bytes).ok()).is_some_and(|commit|commit.tree==root)),"Local changes exist; automatic checkout paused")?;
        }
        return checkout_plan(&store,tip,parent,&reference);
    }
    if input.operation == "status" {
        return Ok(output);
    }
    check(input.operation == "commit", "Unknown Git operation")?;
    check(!input.message.trim().is_empty(), "Enter a commit message")?;
    check(
        !output.changes.is_empty() || parent.is_none(),
        "Nothing to commit",
    )?;
    for identity in [&input.name, &input.email] {
        check(
            !identity.trim().is_empty()
                && !identity
                    .chars()
                    .any(|c| c.is_control() || c == '<' || c == '>'),
            "Enter a valid author name and email",
        )?;
    }
    let signature = format!("{} <{}> {} +0000", input.name, input.email, input.timestamp);
    let commit = crate::objects::CommitData {
        tree: root,
        parents: parent.into_iter().collect(),
        author: signature.clone(),
        committer: signature,
        author_raw: vec![],
        committer_raw: vec![],
        encoding: None,
        message: format!("{}\n", input.message.trim()),
        raw_message: None,
        extra_headers: vec![],
    };
    let tip = store.put(
        ObjectKind::Commit,
        &crate::objects::serialize_commit(&commit),
    )?;
    output.tip = Some(tip.to_hex());
    output.writes = store.emit(&store.new.borrow().clone())?;
    index.sort();
    let mut encoded = Vec::new();
    index.serialize_into(&mut encoded)?;
    let mut hash = crate::hash::ChecksumHasher::new(HashAlgo::Sha1);
    hash.update(&encoded);
    encoded.extend(hash.finalize());
    output
        .writes
        .insert(".git/index".into(), stage(io.clone(), &encoded)?);
    output.writes.insert(
        format!(".git/{reference}"),
        stage(io.clone(), format!("{}\n", tip.to_hex()).as_bytes())?,
    );
    let all = store.walk(tip)?;
    output.bundle = Some(store.bundle(tip, &reference, None, &all)?);
    if let Some(parent) = parent {
        output.incremental_bundle = Some(store.bundle(tip, &reference, Some(parent), &all)?);
    }
    output.ancestors = input
        .known_tips
        .into_iter()
        .filter(|id| ObjectId::from_hex(id).is_ok_and(|id| all.contains(&id) && id != tip))
        .collect();
    (output.snapshot, output.choices) = store.snapshot(tip,true)?;
    output.selected_tip = Some(tip.to_hex());
    Ok(output)
}

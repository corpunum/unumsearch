// SPDX-License-Identifier: Apache-2.0
//! On-disk shard: documents + a sorted trigram table + delta/varint postings.
//!
//! Layout (little endian):
//!   magic[8] "UNSRCH01"
//!   u32 ndocs, u32 ntri, u64 docs_len, u64 table_len, u64 post_len
//!   docs:  repeated { u32 path_len, path bytes, u64 size, i64 mtime_ns }
//!   table: ntri x { u32 trigram, u32 postings_offset }   (sorted by trigram)
//!   postings: per trigram, repeated { varint(doc id gap - 1, from -1), u8 next-byte mask }
//!
//! Shards are written to a temporary name and renamed into place, and opened
//! read-only through mmap: readers never see a partial file, and the resident
//! cost of an index is mostly reclaimable page cache rather than heap.

use crate::trigram::{self, Tri};
use memmap2::Mmap;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

const MAGIC: &[u8; 8] = b"UNSRCH02";
const HEADER: usize = 8 + 4 + 4 + 8 + 8 + 8;

#[derive(Clone, Debug)]
pub struct DocMeta {
    pub rel: String,
    pub size: u64,
    pub mtime_ns: i64,
}

struct Posting {
    last: i64,
    data: Vec<u8>,
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Hasher for trigram keys: one multiply (Fibonacci hashing). The default
/// SipHash is built to resist adversarial keys, which costs most of the
/// time of adding a document; a trigram is three bytes of content, and a
/// file crafted to collide only slows its own indexing.
#[derive(Default, Clone, Copy)]
pub struct TriHasher(u64);

impl std::hash::Hasher for TriHasher {
    fn finish(&self) -> u64 {
        // The high half of the product mixes every key bit; hash tables
        // index by the low bits.
        self.0.rotate_left(32)
    }
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = (self.0.rotate_left(5) ^ *b as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        }
    }
    fn write_u32(&mut self, v: u32) {
        self.0 = (v as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

type TriMap<V> = HashMap<Tri, V, std::hash::BuildHasherDefault<TriHasher>>;

/// Accumulates documents in memory until `budget` bytes of postings, then the
/// caller flushes it to a shard file.
pub struct ShardBuilder {
    docs: Vec<DocMeta>,
    post: TriMap<Posting>,
    bytes: usize,
    scratch: Vec<(Tri, u8)>,
    ex: trigram::Extractor,
}

impl Default for ShardBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ShardBuilder {
    pub fn new() -> Self {
        ShardBuilder {
            docs: Vec::new(),
            post: TriMap::default(),
            bytes: 0,
            scratch: Vec::new(),
            ex: trigram::Extractor::default(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    pub fn docs(&self) -> usize {
        self.docs.len()
    }

    /// Approximate heap held by the builder.
    pub fn memory(&self) -> usize {
        // Hash-map entry + small-vector header + allocator overhead per trigram.
        self.bytes
            + self.post.len() * 80
            + self.docs.len() * 96
            + self.scratch.capacity() * 8
            + self.ex.memory()
    }

    pub fn add(&mut self, meta: DocMeta, content: &[u8]) {
        let id = self.docs.len() as i64;
        self.docs.push(meta);
        self.ex.extract(content, &mut self.scratch);
        for &(t, mask) in &self.scratch {
            let p = self.post.entry(t).or_insert(Posting {
                last: -1,
                data: Vec::new(),
            });
            let before = p.data.capacity();
            put_varint(&mut p.data, (id - p.last - 1) as u64);
            p.data.push(mask);
            p.last = id;
            self.bytes += p.data.capacity() - before;
        }
    }

    pub fn write(self, path: &Path) -> std::io::Result<()> {
        if crate::engine::FAIL_SHARD_WRITES.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(std::io::Error::other("simulated disk write failure"));
        }
        let mut tris: Vec<(&Tri, &Posting)> = self.post.iter().collect();
        tris.sort_unstable_by_key(|(t, _)| **t);

        let mut docs = Vec::new();
        for d in &self.docs {
            docs.extend_from_slice(&(d.rel.len() as u32).to_le_bytes());
            docs.extend_from_slice(d.rel.as_bytes());
            docs.extend_from_slice(&d.size.to_le_bytes());
            docs.extend_from_slice(&d.mtime_ns.to_le_bytes());
        }
        let mut table = Vec::with_capacity(tris.len() * 8);
        let mut off: u64 = 0;
        for (t, p) in &tris {
            table.extend_from_slice(&t.to_le_bytes());
            table.extend_from_slice(&(off as u32).to_le_bytes());
            off += p.data.len() as u64;
        }
        if off > u32::MAX as u64 {
            return Err(std::io::Error::other("shard postings exceed 4 GiB"));
        }
        let tmp = path.with_extension("tmp");
        {
            let mut w = BufWriter::new(File::create(&tmp)?);
            w.write_all(MAGIC)?;
            w.write_all(&(self.docs.len() as u32).to_le_bytes())?;
            w.write_all(&(tris.len() as u32).to_le_bytes())?;
            w.write_all(&(docs.len() as u64).to_le_bytes())?;
            w.write_all(&(table.len() as u64).to_le_bytes())?;
            w.write_all(&off.to_le_bytes())?;
            w.write_all(&docs)?;
            w.write_all(&table)?;
            for (_, p) in &tris {
                w.write_all(&p.data)?;
            }
            w.flush()?;
            w.get_ref().sync_all().ok();
        }
        std::fs::rename(&tmp, path)
    }
}

/// A document entry read in place from the mapped shard (no per-document
/// heap allocation: the table stays in reclaimable page cache).
#[derive(Clone, Copy, Debug)]
pub struct DocRef<'a> {
    pub rel: &'a str,
    pub size: u64,
    pub mtime_ns: i64,
}

pub struct Shard {
    path: std::path::PathBuf,
    map: Mmap,
    /// Byte offset of each document record inside the map.
    doc_offs: Vec<u32>,
    ntri: usize,
    table_off: usize,
    post_off: usize,
    post_len: usize,
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

impl Shard {
    pub fn open(path: &Path) -> std::io::Result<Shard> {
        let f = File::open(path)?;
        // SAFETY: shards are immutable once renamed into place; writers only
        // ever create new files.
        let map = unsafe { Mmap::map(&f)? };
        let bad = || std::io::Error::new(std::io::ErrorKind::InvalidData, "corrupt shard");
        if map.len() < HEADER || &map[..8] != MAGIC {
            return Err(bad());
        }
        let ndocs = u32_at(&map, 8) as usize;
        let ntri = u32_at(&map, 12) as usize;
        let docs_len = u64_at(&map, 16) as usize;
        let table_len = u64_at(&map, 24) as usize;
        let post_len = u64_at(&map, 32) as usize;
        let table_off = HEADER + docs_len;
        let post_off = table_off + table_len;
        if post_off + post_len != map.len() || table_len != ntri * 8 {
            return Err(bad());
        }
        if ndocs > docs_len / 20 + 1 {
            return Err(bad());
        }
        let mut doc_offs = Vec::with_capacity(ndocs);
        let mut o = HEADER;
        for _ in 0..ndocs {
            if o + 4 > table_off {
                return Err(bad());
            }
            let n = u32_at(&map, o) as usize;
            if o + 4 + n + 16 > table_off || std::str::from_utf8(&map[o + 4..o + 4 + n]).is_err() {
                return Err(bad());
            }
            doc_offs.push(o as u32);
            o += 4 + n + 16;
        }
        if o != table_off {
            return Err(bad());
        }
        // Validation touched every document record; give those pages back so
        // a freshly opened index does not count as resident (they stay in the
        // page cache and fault back in when a query needs them).
        #[cfg(unix)]
        {
            // SAFETY: a read-only file-backed mapping; dropped pages are
            // simply faulted in again from the file.
            let _ = unsafe { map.unchecked_advise(memmap2::UncheckedAdvice::DontNeed) };
        }
        Ok(Shard {
            path: path.to_path_buf(),
            map,
            doc_offs,
            ntri,
            table_off,
            post_off,
            post_len,
        })
    }

    pub fn ndocs(&self) -> usize {
        self.doc_offs.len()
    }

    /// The document with this id, or `None` if the id is out of range.
    pub fn doc(&self, id: usize) -> Option<DocRef<'_>> {
        let o = *self.doc_offs.get(id)? as usize;
        let n = u32_at(&self.map, o) as usize;
        let rel = std::str::from_utf8(&self.map[o + 4..o + 4 + n]).ok()?;
        let e = o + 4 + n;
        Some(DocRef {
            rel,
            size: u64_at(&self.map, e),
            mtime_ns: u64_at(&self.map, e + 8) as i64,
        })
    }

    pub fn docs(&self) -> impl Iterator<Item = DocRef<'_>> + '_ {
        (0..self.ndocs()).filter_map(move |i| self.doc(i))
    }

    pub fn disk_bytes(&self) -> usize {
        self.map.len()
    }

    /// Where the shard file is.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn postings(&self, t: Tri) -> Option<&[u8]> {
        let (mut lo, mut hi) = (0usize, self.ntri);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let v = u32_at(&self.map, self.table_off + mid * 8);
            if v < t {
                lo = mid + 1;
            } else if v > t {
                hi = mid;
            } else {
                let start = u32_at(&self.map, self.table_off + mid * 8 + 4) as usize;
                let end = if mid + 1 < self.ntri {
                    u32_at(&self.map, self.table_off + (mid + 1) * 8 + 4) as usize
                } else {
                    self.post_len
                };
                if start > end || end > self.post_len {
                    return None;
                }
                return Some(&self.map[self.post_off + start..self.post_off + end]);
            }
        }
        None
    }

    /// Doc ids of a posting list, keeping only entries whose next-byte mask
    /// intersects `need` (0 = keep all).
    fn decode(bytes: &[u8], need: u8) -> Vec<u32> {
        let mut out = Vec::new();
        let mut cur: i64 = -1;
        let (mut v, mut shift) = (0u64, 0u32);
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            i += 1;
            v |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                cur += v as i64 + 1;
                let mask = bytes.get(i).copied().unwrap_or(0);
                i += 1;
                if need == 0 || mask & need != 0 {
                    out.push(cur as u32);
                }
                v = 0;
                shift = 0;
            } else {
                shift += 7;
            }
        }
        out
    }

    /// Candidate doc ids for a plan; `None` means "every document".
    pub fn eval(&self, p: &trigram::Plan) -> Option<Vec<u32>> {
        use trigram::Plan;
        match p {
            Plan::All => None,
            Plan::Tri(t) => Some(
                self.postings(*t)
                    .map(|b| Self::decode(b, 0))
                    .unwrap_or_default(),
            ),
            Plan::TriNext(t, n) => Some(
                self.postings(*t)
                    .map(|b| Self::decode(b, trigram::next_bit(*n)))
                    .unwrap_or_default(),
            ),
            Plan::And(xs) => {
                let mut acc: Option<Vec<u32>> = None;
                for x in xs {
                    if let Some(v) = self.eval(x) {
                        acc = Some(match acc {
                            None => v,
                            Some(a) => intersect(&a, &v),
                        });
                        if acc.as_ref().is_some_and(|a| a.is_empty()) {
                            break;
                        }
                    }
                }
                acc
            }
            Plan::Or(xs) => {
                let mut acc: Vec<u32> = Vec::new();
                for x in xs {
                    let v = self.eval(x)?;
                    acc = union(&acc, &v);
                }
                Some(acc)
            }
        }
    }
}

/// One input of [`merge`]: a shard and which of its documents to keep
/// (`None` = all of them).
pub struct MergeInput<'a> {
    pub shard: &'a Shard,
    pub alive: Option<&'a [bool]>,
}

/// Sequential reader of one input's trigram table and postings, through
/// buffered `read`s rather than the mapping: a merge streams every posting
/// list once, and reading it through the map would leave the whole shard
/// resident in this process (and counted in its RSS).
struct Cursor {
    table: std::io::BufReader<File>,
    post: std::io::BufReader<File>,
    ntri: usize,
    post_len: usize,
    /// Index of the table entry `cur` is, and the one after it.
    at: usize,
    cur: Option<(Tri, u32)>,
    next: Option<(Tri, u32)>,
}

impl Cursor {
    fn open(s: &Shard) -> std::io::Result<Cursor> {
        use std::io::{Seek, SeekFrom};
        let mut t = File::open(&s.path)?;
        t.seek(SeekFrom::Start(s.table_off as u64))?;
        let mut p = File::open(&s.path)?;
        p.seek(SeekFrom::Start(s.post_off as u64))?;
        let mut c = Cursor {
            table: std::io::BufReader::with_capacity(1 << 16, t),
            post: std::io::BufReader::with_capacity(1 << 16, p),
            ntri: s.ntri,
            post_len: s.post_len,
            at: 0,
            cur: None,
            next: None,
        };
        c.cur = c.read_entry()?;
        c.next = c.read_entry()?;
        Ok(c)
    }

    fn read_entry(&mut self) -> std::io::Result<Option<(Tri, u32)>> {
        use std::io::Read;
        if self.at >= self.ntri {
            return Ok(None);
        }
        self.at += 1;
        let mut b = [0u8; 8];
        self.table.read_exact(&mut b)?;
        Ok(Some((u32_at(&b, 0), u32_at(&b, 4))))
    }

    /// Advance past the current trigram without reading its postings.
    fn skip(&mut self) -> std::io::Result<()> {
        self.cur = self.next;
        self.next = self.read_entry()?;
        Ok(())
    }

    /// The posting list of the current trigram; advances to the next.
    fn take(&mut self, out: &mut Vec<u8>) -> std::io::Result<()> {
        use std::io::Read;
        let (_, start) = self.cur.expect("cursor past the end");
        let end = self.next.map_or(self.post_len as u32, |(_, o)| o);
        if end < start {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "corrupt shard",
            ));
        }
        out.resize((end - start) as usize, 0);
        self.post.read_exact(out)?;
        self.cur = self.next;
        self.next = self.read_entry()?;
        Ok(())
    }
}

/// Decode a posting list into (doc id, next-byte mask) pairs.
fn decode_masks(bytes: &[u8], out: &mut Vec<(u32, u8)>) {
    out.clear();
    let mut cur: i64 = -1;
    let (mut v, mut shift) = (0u64, 0u32);
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        i += 1;
        v |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            cur += v as i64 + 1;
            out.push((cur as u32, bytes.get(i).copied().unwrap_or(0)));
            i += 1;
            v = 0;
            shift = 0;
        } else {
            shift += 7;
        }
    }
}

/// Write one shard holding the kept documents of `inputs`, in path order,
/// without reading any source file: posting lists are decoded, renumbered
/// and re-encoded. Used to drop deleted or changed documents from a unit's
/// shards (an incremental rebuild) and to combine small shards. A path kept
/// in more than one input is taken from the last of them. Returns the number
/// of documents written.
pub fn merge(inputs: &[MergeInput<'_>], path: &Path) -> std::io::Result<usize> {
    use std::io::{Seek, SeekFrom};
    if crate::engine::FAIL_SHARD_WRITES.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(std::io::Error::other("simulated disk write failure"));
    }
    // The kept documents, sorted by path; a later input wins a duplicate.
    let mut docs: Vec<(DocRef<'_>, usize, u32)> = Vec::new();
    for (k, inp) in inputs.iter().enumerate() {
        for id in 0..inp.shard.ndocs() {
            if inp
                .alive
                .is_some_and(|a| !a.get(id).copied().unwrap_or(false))
            {
                continue;
            }
            if let Some(d) = inp.shard.doc(id) {
                docs.push((d, k, id as u32));
            }
        }
    }
    docs.sort_by(|a, b| a.0.rel.cmp(b.0.rel).then(a.1.cmp(&b.1)));
    let mut keep = vec![true; docs.len()];
    for i in 1..docs.len() {
        if docs[i - 1].0.rel == docs[i].0.rel {
            keep[i - 1] = false;
        }
    }
    let mut remap: Vec<Vec<u32>> = inputs
        .iter()
        .map(|i| vec![u32::MAX; i.shard.ndocs()])
        .collect();
    let mut docbuf = Vec::new();
    let mut n = 0u32;
    for (i, (d, k, id)) in docs.iter().enumerate() {
        if !keep[i] {
            continue;
        }
        remap[*k][*id as usize] = n;
        n += 1;
        docbuf.extend_from_slice(&(d.rel.len() as u32).to_le_bytes());
        docbuf.extend_from_slice(d.rel.as_bytes());
        docbuf.extend_from_slice(&d.size.to_le_bytes());
        docbuf.extend_from_slice(&d.mtime_ns.to_le_bytes());
    }
    let ndocs = n as usize;
    drop(docs);

    // Trigrams of the output: the union of the input tables (a trigram whose
    // documents were all dropped keeps an empty posting list; harmless).
    let mut cursors = Vec::with_capacity(inputs.len());
    for inp in inputs {
        cursors.push(Cursor::open(inp.shard)?);
    }
    // The union of the input tables, counted by a merge of their (sorted)
    // trigram streams.
    let ntri = {
        let mut cs = Vec::with_capacity(inputs.len());
        for inp in inputs {
            cs.push(Cursor::open(inp.shard)?);
        }
        let mut n = 0usize;
        while let Some(t) = cs.iter().filter_map(|c| c.cur.map(|x| x.0)).min() {
            n += 1;
            for c in cs.iter_mut() {
                if c.cur.map(|x| x.0) == Some(t) {
                    c.skip()?;
                }
            }
        }
        n
    };

    let tmp = path.with_extension("tmp");
    let res = (|| -> std::io::Result<()> {
        let mut w = std::io::BufWriter::with_capacity(1 << 20, File::create(&tmp)?);
        w.write_all(MAGIC)?;
        w.write_all(&(ndocs as u32).to_le_bytes())?;
        w.write_all(&(ntri as u32).to_le_bytes())?;
        w.write_all(&(docbuf.len() as u64).to_le_bytes())?;
        w.write_all(&((ntri * 8) as u64).to_le_bytes())?;
        w.write_all(&0u64.to_le_bytes())?; // post_len, patched below
        w.write_all(&docbuf)?;
        let table_off = (HEADER + docbuf.len()) as u64;
        w.seek(SeekFrom::Start(table_off + (ntri * 8) as u64))?;
        // The table is streamed through a second handle on the same file.
        let mut tw = std::io::BufWriter::with_capacity(
            1 << 16,
            std::fs::OpenOptions::new().write(true).open(&tmp)?,
        );
        tw.seek(SeekFrom::Start(table_off))?;
        let mut emitted = 0usize;
        let (mut raw, mut ids, mut merged, mut enc) =
            (Vec::new(), Vec::new(), Vec::<(u32, u8)>::new(), Vec::new());
        let mut off: u64 = 0;
        while let Some(t) = cursors.iter().filter_map(|c| c.cur.map(|x| x.0)).min() {
            merged.clear();
            let mut sources = 0;
            for (k, c) in cursors.iter_mut().enumerate() {
                if c.cur.map(|x| x.0) != Some(t) {
                    continue;
                }
                c.take(&mut raw)?;
                decode_masks(&raw, &mut ids);
                let before = merged.len();
                for &(id, m) in &ids {
                    if let Some(&nid) = remap[k].get(id as usize) {
                        if nid != u32::MAX {
                            merged.push((nid, m));
                        }
                    }
                }
                if merged.len() > before {
                    sources += 1;
                }
            }
            if sources > 1 {
                merged.sort_unstable_by_key(|x| x.0);
            }
            enc.clear();
            let mut last: i64 = -1;
            for &(id, m) in &merged {
                put_varint(&mut enc, (id as i64 - last - 1) as u64);
                enc.push(m);
                last = id as i64;
            }
            tw.write_all(&t.to_le_bytes())?;
            tw.write_all(&(off as u32).to_le_bytes())?;
            emitted += 1;
            off += enc.len() as u64;
            if off > u32::MAX as u64 {
                return Err(std::io::Error::other("shard postings exceed 4 GiB"));
            }
            w.write_all(&enc)?;
        }
        if emitted != ntri {
            return Err(std::io::Error::other("merge: trigram count changed"));
        }
        tw.flush()?;
        tw.get_ref().sync_all().ok();
        drop(tw);
        w.seek(SeekFrom::Start(32))?;
        w.write_all(&off.to_le_bytes())?;
        w.flush()?;
        w.get_ref().sync_all().ok();
        Ok(())
    })();
    if let Err(e) = res {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, path)?;
    Ok(ndocs)
}

fn intersect(a: &[u32], b: &[u32]) -> Vec<u32> {
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out
}

fn union(a: &[u32], b: &[u32]) -> Vec<u32> {
    let (mut i, mut j, mut out) = (0, 0, Vec::with_capacity(a.len() + b.len()));
    while i < a.len() || j < b.len() {
        if j >= b.len() || (i < a.len() && a[i] < b[j]) {
            out.push(a[i]);
            i += 1;
        } else if i >= a.len() || b[j] < a[i] {
            out.push(b[j]);
            j += 1;
        } else {
            out.push(a[i]);
            i += 1;
            j += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_query() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = ShardBuilder::new();
        let docs = ["hello world", "say hello", "nothing here", "HELLO there"];
        for (i, d) in docs.iter().enumerate() {
            b.add(
                DocMeta {
                    rel: format!("f{i}.txt"),
                    size: d.len() as u64,
                    mtime_ns: 0,
                },
                d.as_bytes(),
            );
        }
        let p = dir.path().join("s.shard");
        b.write(&p).unwrap();
        let s = Shard::open(&p).unwrap();
        assert_eq!(s.ndocs(), 4);
        let plan = trigram::plan("hello", true).unwrap();
        assert_eq!(s.eval(&plan), Some(vec![0, 1, 3]));
        let plan = trigram::plan("world|there", false).unwrap();
        assert_eq!(s.eval(&plan), Some(vec![0, 3]));
        assert_eq!(s.eval(&trigram::plan("zzz", false).unwrap()), Some(vec![]));
    }

    #[test]
    fn next_byte_mask_rejects_non_adjacent_trigrams() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = ShardBuilder::new();
        // Both contain the trigrams of "update" + "avail" pieces; only one
        // contains them adjacently.
        for (i, d) in ["update; available", "updateAvailable"].iter().enumerate() {
            b.add(
                DocMeta {
                    rel: format!("{i}"),
                    size: 0,
                    mtime_ns: 0,
                },
                d.as_bytes(),
            );
        }
        let p = dir.path().join("m.shard");
        b.write(&p).unwrap();
        let s = Shard::open(&p).unwrap();
        assert_eq!(
            s.eval(&trigram::plan("updateavailable", true).unwrap()),
            Some(vec![1])
        );
        assert_eq!(
            s.eval(&trigram::plan("update", true).unwrap()),
            Some(vec![0, 1])
        );
    }

    fn build(dir: &Path, name: &str, docs: &[(&str, &str)]) -> Shard {
        let mut b = ShardBuilder::new();
        for (rel, body) in docs {
            b.add(
                DocMeta {
                    rel: rel.to_string(),
                    size: body.len() as u64,
                    mtime_ns: 7,
                },
                body.as_bytes(),
            );
        }
        let p = dir.join(name);
        b.write(&p).unwrap();
        Shard::open(&p).unwrap()
    }

    fn rels(s: &Shard, ids: Option<Vec<u32>>) -> Vec<String> {
        match ids {
            Some(v) => v
                .into_iter()
                .map(|i| s.doc(i as usize).unwrap().rel.to_string())
                .collect(),
            None => s.docs().map(|d| d.rel.to_string()).collect(),
        }
    }

    #[test]
    fn merge_drops_dead_documents_and_equals_a_fresh_build() {
        let dir = tempfile::tempdir().unwrap();
        let a = build(
            dir.path(),
            "a.shard",
            &[
                ("a/1", "hello world"),
                ("c/3", "updateAvailable"),
                ("d/4", "gone soon"),
            ],
        );
        let b = build(
            dir.path(),
            "b.shard",
            &[
                ("b/2", "say hello"),
                ("c/3", "update; available"),
                ("e/5", ""),
            ],
        );
        // c/3 is in both: the later input wins. d/4 is dropped.
        let alive_a = [true, true, false];
        let out = dir.path().join("m.shard");
        let n = merge(
            &[
                MergeInput {
                    shard: &a,
                    alive: Some(&alive_a),
                },
                MergeInput {
                    shard: &b,
                    alive: None,
                },
            ],
            &out,
        )
        .unwrap();
        assert_eq!(n, 4);
        let m = Shard::open(&out).unwrap();
        let want = build(
            dir.path(),
            "w.shard",
            &[
                ("a/1", "hello world"),
                ("b/2", "say hello"),
                ("c/3", "update; available"),
                ("e/5", ""),
            ],
        );
        // Documents in path order, same metadata.
        assert_eq!(rels(&m, None), rels(&want, None));
        for pat in [
            "hello",
            "updateavailable",
            "update",
            "gone",
            "world|say",
            "zzz",
        ] {
            let p = trigram::plan(pat, true).unwrap();
            assert_eq!(rels(&m, m.eval(&p)), rels(&want, want.eval(&p)), "{pat}");
        }
        // Merging nothing alive gives an empty, valid shard.
        let none = [false, false, false];
        let out2 = dir.path().join("empty.shard");
        assert_eq!(
            merge(
                &[MergeInput {
                    shard: &a,
                    alive: Some(&none)
                }],
                &out2
            )
            .unwrap(),
            0
        );
        let e = Shard::open(&out2).unwrap();
        assert_eq!(e.ndocs(), 0);
        assert_eq!(e.eval(&trigram::plan("hello", true).unwrap()), Some(vec![]));
    }
}

// SPDX-License-Identifier: Apache-2.0
//! The engine: units, the manifest, building, and searching.
//!
//! A *unit* is a directory indexed as one group of shards (each configured
//! root, or each child of a split root). Exactly one process per index
//! directory is the writer (it holds `writer.lock`); any number of readers
//! (CLI invocations, MCP servers) open the same index read-only and reload it
//! when the manifest changes.

use crate::config::Config;
use crate::shard::{DocMeta, Shard, ShardBuilder};
use crate::trigram;
use crate::walk::{self, FileEntry};
use ignore::overrides::{Override, OverrideBuilder};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UnitState {
    pub path: String,
    pub shards: Vec<String>,
    pub fingerprint: u64,
    pub files: usize,
    pub bytes: u64,
    pub binary_skipped: usize,
    pub indexed_at_ms: u64,
    pub build_ms: u64,
    pub ready: bool,
    pub dirty: bool,
    pub dirty_since_ms: u64,
    #[serde(skip)]
    pub last_event_ms: u64,
    pub watched: bool,
    /// Files changed since the last build began (relative paths). While a
    /// unit is dirty, searches also verify these directly, so answers stay
    /// exact without waiting for the rebuild.
    #[serde(default)]
    pub pending: std::collections::BTreeSet<String>,
    /// A change the overlay cannot represent (directory moves, ignore-file
    /// edits, lost events): the unit is not fresh until rebuilt.
    #[serde(default)]
    pub pending_unknown: bool,
    /// A rebuild of this unit is running. Its listing may predate changes
    /// that arrive meanwhile, so those are also recorded below; the state
    /// above stays exactly as it was (and visible to queries) until the
    /// rebuild publishes its shards (issue #6).
    #[serde(skip)]
    pub building: bool,
    /// Files changed since the running rebuild began.
    #[serde(skip)]
    pub build_pending: std::collections::BTreeSet<String>,
    /// An unrepresentable change arrived since the running rebuild began.
    #[serde(skip)]
    pub build_unknown: bool,
    /// When the first change since the running rebuild began arrived.
    #[serde(skip)]
    pub build_dirty_since_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// For a split root's own unit: directories directly inside it that its
    /// corpus contains but no unit indexes (hidden names), absolute paths.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncovered: Vec<String>,
    /// Consecutive failed rebuilds (drives the retry back-off).
    #[serde(skip)]
    pub failures: u32,
    /// Do not retry a failed rebuild before this time.
    #[serde(skip)]
    pub retry_at_ms: u64,
}

/// What a filesystem event changed in a unit.
#[derive(Clone, Debug)]
pub enum Change {
    File(String),
    Unknown,
}

const MAX_PENDING: usize = 2000;

/// Most threads one rebuild uses (reading files and merging shards). Each
/// reading thread also gets at least [`BUILD_THREAD_MB`] of the memory
/// budget, so the default budget (96 MiB) allows 4.
const MAX_BUILD_THREADS: usize = 8;
const BUILD_THREAD_MB: u64 = 24;

/// A file whose change stamp is within this of the moment it was read is
/// "racily clean": see [`Engine::build_pieces`].
const RACY_NS: i64 = 2_000_000_000;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub heartbeat_ms: u64,
    pub rescan_secs: u64,
    pub units: BTreeMap<String, UnitState>,
}

#[derive(Clone, Debug, Serialize)]
pub struct UnitStatus {
    pub path: String,
    pub ready: bool,
    pub dirty: bool,
    pub watched: bool,
    pub indexed_at_ms: u64,
    pub files: usize,
}

impl From<&UnitState> for UnitStatus {
    fn from(u: &UnitState) -> Self {
        UnitStatus {
            path: u.path.clone(),
            ready: u.ready,
            dirty: u.dirty,
            watched: u.watched,
            indexed_at_ms: u.indexed_at_ms,
            files: u.files,
        }
    }
}

struct Inner {
    m: Manifest,
    shards: HashMap<String, Arc<Vec<Shard>>>,
}

pub struct Engine {
    pub cfg: Config,
    dir: PathBuf,
    inner: RwLock<Inner>,
    build_lock: Mutex<()>,
    writer: bool,
    _lock: Option<File>,
    manifest_stamp: Mutex<Option<(SystemTime, u64, u64)>>,
    pub(crate) watched_dirs: Mutex<HashSet<PathBuf>>,
    pub(crate) need_unit_scan: AtomicBool,
    doc_sets: Mutex<HashMap<String, Arc<HashSet<String>>>>,
    /// Last shard generation handed out (strictly increasing).
    last_gen: AtomicU64,
    /// Configured roots in both spellings (as written, canonical).
    anchors: OnceLock<Vec<(PathBuf, PathBuf)>>,
    /// Split roots, normalised.
    splits: OnceLock<Vec<PathBuf>>,
    /// Queries running now (see [`Engine::query_slot`]).
    running: (Mutex<usize>, std::sync::Condvar),
    /// Whether the corpus walk of a unit reaches a sub-directory, keyed by
    /// (unit, sub) and valid for the unit fingerprint it was computed at.
    reach_cache: Mutex<HashMap<(String, String), (u64, bool)>>,
    /// Test hook: sleep this long inside every rebuild, after the listing
    /// and before the new shards are published (emulates a slow disk, e.g.
    /// `F_FULLFSYNC` on macOS, which makes the rebuild window long).
    build_stall_ms: AtomicU64,
    /// Shard size the merge aims for (test hook: [`Engine::set_build_sizes`]).
    shard_target: AtomicU64,
    /// Test hook: flush a reading thread's pieces at this many bytes (0: from
    /// the memory budget).
    piece_flush: AtomicU64,
    /// See [`RACY_NS`] (test hook: [`Engine::set_racy_window_ns`]).
    racy_ns: AtomicU64,
}

/// A held query slot; dropping it lets a waiting query run.
pub struct QuerySlot<'a>(&'a Engine);

impl Drop for QuerySlot<'_> {
    fn drop(&mut self) {
        let (m, cv) = &self.0.running;
        *m.lock().unwrap() -= 1;
        cv.notify_one();
    }
}

/// Test hook: make shard writes fail (simulates a full or failing disk).
#[doc(hidden)]
pub static FAIL_SHARD_WRITES: AtomicBool = AtomicBool::new(false);

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn unit_prefix(unit: &str) -> String {
    // FNV-1a of the unit path: stable, filesystem-safe shard names.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in unit.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("u{h:016x}")
}

#[derive(Clone, Debug)]
pub struct SearchOpts {
    pub pattern: String,
    pub root: PathBuf,
    pub regex: bool,
    pub case_insensitive: bool,
    pub globs: Vec<String>,
    pub max_matches: usize,
    pub max_files: usize,
    pub files_only: bool,
    /// Return trigram candidates without verifying them (for clients that
    /// verify with their own regex dialect).
    pub candidates_only: bool,
    /// When the root is not covered by the index, walk and scan it instead.
    pub scan_fallback: bool,
    /// Memory budget for the returned paths and lines; past it the answer is
    /// cut (deterministically, in path order) and marked truncated.
    pub max_result_bytes: usize,
}

impl Default for SearchOpts {
    fn default() -> Self {
        SearchOpts {
            pattern: String::new(),
            root: PathBuf::from("."),
            regex: false,
            case_insensitive: false,
            globs: vec![],
            max_matches: 1000,
            max_files: 20_000,
            files_only: false,
            candidates_only: false,
            scan_fallback: true,
            max_result_bytes: DEFAULT_RESULT_BYTES,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Match {
    pub path: String,
    pub line: u64,
    pub text: String,
}

/// A part of the requested root that the index does not cover.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Uncovered {
    pub path: String,
    /// Why, and what a caller may do about it:
    /// - `"excluded"`: the index leaves the path out (ignore files, excludes,
    ///   hidden-file rules, the size cap, a symlink) - scan it to search it;
    /// - `"not_indexed"`: no configured root (or no unit) contains it - scan it;
    /// - `"secret"`: a secret location (`.ssh`, `.env`, keys, ...) - never
    ///   indexed and never scanned; do not search it with another tool either.
    pub reason: &'static str,
}

impl Uncovered {
    fn new(p: &Path, reason: &'static str) -> Self {
        Uncovered {
            path: path_str(p),
            reason,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct SearchResult {
    pub backend: &'static str,
    /// The index (with its overlay of changed files) contains every file of
    /// the root's corpus; see RELIABILITY.md. `false` whenever any part of
    /// the root is outside the index, listed in `uncovered`.
    pub covered: bool,
    pub fresh: bool,
    pub files: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub matches: Vec<Match>,
    pub candidates: usize,
    pub truncated: bool,
    /// `covered && fresh && !truncated`: the answer is exactly what a full
    /// scan of the root would return now.
    pub complete: bool,
    pub units: Vec<UnitStatus>,
    /// The parts of the root the index does not cover (empty when `covered`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub uncovered: Vec<Uncovered>,
    pub elapsed_ms: f64,
}

#[derive(Clone, Debug)]
pub struct FilesOpts {
    pub root: PathBuf,
    pub globs: Vec<String>,
    /// Regex tested against the path relative to root, and the basename.
    pub regex: Option<String>,
    pub max_files: usize,
    pub scan_fallback: bool,
    /// Memory budget for the returned paths (see [`SearchOpts`]).
    pub max_result_bytes: usize,
}

impl Default for FilesOpts {
    fn default() -> Self {
        FilesOpts {
            root: PathBuf::from("."),
            globs: vec![],
            regex: None,
            max_files: 5000,
            scan_fallback: true,
            max_result_bytes: DEFAULT_RESULT_BYTES,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct FilesResult {
    pub backend: &'static str,
    pub covered: bool,
    pub fresh: bool,
    pub files: Vec<String>,
    pub truncated: bool,
    /// `covered && fresh && !truncated` (see [`SearchResult::complete`]).
    pub complete: bool,
    pub units: Vec<UnitStatus>,
    /// The parts of the root the index does not cover (empty when `covered`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub uncovered: Vec<Uncovered>,
    pub elapsed_ms: f64,
}

impl SearchResult {
    /// What this answer counts against a result budget.
    pub fn result_bytes(&self) -> usize {
        let files: usize = self.files.iter().map(|f| f.len() + ITEM_OVERHEAD).sum();
        let lines: usize = self
            .matches
            .iter()
            .map(|m| m.path.len() + m.text.len() + ITEM_OVERHEAD)
            .sum();
        if self.matches.is_empty() {
            files
        } else {
            lines
        }
    }
}

/// Default per-query result budget (the `max_result_mb` default).
pub const DEFAULT_RESULT_BYTES: usize = 48 << 20;

/// Bytes a returned path or line costs beyond its text (String header,
/// allocator slack, JSON quoting), for the result budget.
const ITEM_OVERHEAD: usize = 48;

/// Candidate count from which verification may use more threads.
const BIG_VERIFY: usize = 16 * 1024;

/// Candidates a verification worker claims at a time.
const VERIFY_BLOCK: usize = 16;

/// One candidate file: a unit (index into the covering roots) and a path
/// relative to it, borrowed from the mmapped document table when it comes
/// from the index. Kept compact so a query whose candidates are most of the
/// corpus does not hold an absolute `PathBuf` per file.
struct Cand<'a> {
    unit: u32,
    rel: std::borrow::Cow<'a, str>,
}

/// A rebuild succeeded (its shards are live, or its listing matched the
/// index): what was pending when it began is resolved; only changes that
/// arrived while it ran remain pending.
fn finish_build(u: &mut UnitState) {
    u.building = false;
    u.pending = std::mem::take(&mut u.build_pending);
    u.pending_unknown = std::mem::take(&mut u.build_unknown);
    u.dirty = !u.pending.is_empty() || u.pending_unknown;
    if u.dirty {
        u.dirty_since_ms = u.build_dirty_since_ms;
    }
    u.build_dirty_since_ms = 0;
}

/// What an incremental rebuild keeps and what it reads again.
struct Plan {
    /// Per old shard, per document: still current (path, size and mtime as
    /// listed, and not reported changed by an event).
    alive: Vec<Vec<bool>>,
    /// How many documents are kept, and their content bytes.
    alive_docs: usize,
    alive_bytes: u64,
    /// Indices into the listing of the files to (re)index.
    reindex: Vec<usize>,
}

/// Compare a listing with the documents of the current shards. A document
/// stays when the listing has its path with the same size and mtime and no
/// event reported it changed (`pending`: an edit within the filesystem's
/// timestamp granularity keeps the mtime, the event still says it changed).
/// Everything else in the listing is read again; documents not kept are
/// dropped (deleted, changed, or no longer part of the corpus).
fn incremental_plan(
    old: &[Shard],
    files: &[FileEntry],
    pending: &std::collections::BTreeSet<String>,
) -> Plan {
    let mut at: HashMap<&str, (usize, u32, u64, i64)> = HashMap::new();
    for (k, s) in old.iter().enumerate() {
        for id in 0..s.ndocs() {
            if let Some(d) = s.doc(id) {
                at.insert(d.rel, (k, id as u32, d.size, d.mtime_ns));
            }
        }
    }
    let mut alive: Vec<Vec<bool>> = old.iter().map(|s| vec![false; s.ndocs()]).collect();
    let mut reindex = Vec::new();
    let (mut alive_docs, mut alive_bytes) = (0usize, 0u64);
    for (i, f) in files.iter().enumerate() {
        match at.get(f.rel.as_str()) {
            Some(&(k, id, size, mtime))
                if size == f.size && mtime == f.mtime_ns && !pending.contains(&f.rel) =>
            {
                alive[k][id as usize] = true;
                alive_docs += 1;
                alive_bytes += size;
            }
            _ => reindex.push(i),
        }
    }
    Plan {
        alive,
        alive_docs,
        alive_bytes,
        reindex,
    }
}

/// Shards are combined up to about this size on disk: a few large shards
/// per unit (a query looks every trigram up once per shard), small enough
/// that rewriting the one holding a changed file stays cheap.
const SHARD_TARGET: u64 = 64 << 20;

/// Clean shards under half of [`SHARD_TARGET`] a unit may keep before a
/// rebuild combines them.
const MAX_SMALL_SHARDS: usize = 8;

/// A source of candidates: a unit's path, its shards and its pending
/// (changed since build) files, snapshotted under the lock.
struct Src {
    path: PathBuf,
    shards: Option<Arc<Vec<Shard>>>,
    pending: Vec<String>,
}

/// Order of '/'-separated relative paths that equals `Path` component order
/// (what sorting absolute `PathBuf`s gave before): '/' sorts below every byte.
fn rel_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (x, y) = (a.as_bytes(), b.as_bytes());
    match x.iter().zip(y).position(|(p, q)| p != q) {
        None => x.len().cmp(&y.len()),
        Some(i) => {
            let k = |c: u8| if c == b'/' { 0u16 } else { c as u16 + 1 };
            k(x[i]).cmp(&k(y[i]))
        }
    }
}

/// What verification found in one file.
enum Hit {
    File(String),
    Lines(Vec<Match>),
}

/// Read `p` into `buf` if it is at most `cap` bytes (the corpus size cap: a
/// file that grew past it is skipped, as a walk would skip it). The buffer is
/// reused across files.
fn read_capped(p: &Path, cap: u64, buf: &mut Vec<u8>) -> bool {
    use std::io::Read;
    buf.clear();
    let Ok(f) = File::open(p) else { return false };
    let hint = f.metadata().map(|m| m.len()).unwrap_or(0);
    if hint > cap {
        return false;
    }
    buf.reserve(hint as usize + 1);
    match f.take(cap + 1).read_to_end(buf) {
        Ok(_) => buf.len() as u64 <= cap,
        Err(_) => false,
    }
}

/// Does any line of `buf` match (the same answer as testing every line, as
/// the per-line loop does)? The whole-buffer search jumps straight to a
/// candidate line; only that line is then confirmed.
fn file_has_match(re: &regex::bytes::Regex, buf: &[u8]) -> bool {
    // The earliest end of any match costs about what `is_match` does (no
    // search for the start); the line holding that end is the candidate.
    let Some(e) = re.shortest_match(buf) else {
        return false;
    };
    let at = e.saturating_sub(1).min(buf.len().saturating_sub(1));
    let start = buf[..at]
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(0, |i| i + 1);
    let end = buf[at..]
        .iter()
        .position(|b| *b == b'\n')
        .map_or(buf.len(), |i| at + i);
    let line = &buf[start..end];
    if re.is_match(line.strip_suffix(b"\r").unwrap_or(line)) {
        return true;
    }
    // The buffer-level match was not a line-level one (it spans a newline,
    // or depends on `\r` / buffer edges): fall back to every line.
    buf.split(|b| *b == b'\n').any(|line| {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        re.is_match(line)
    })
}

struct Cover {
    units: Vec<String>,
    /// '/'-separated path of root inside the single covering unit ("" = whole unit).
    sub: String,
    /// Parts of the root no unit covers (a split root's non-unit children).
    uncovered: Vec<Uncovered>,
}

/// Does the unit's index hold a document at `sub` or below it? Documents are
/// stored in path order, so this is a binary search per shard.
fn shards_have_sub(shards: &[Shard], sub: &str) -> bool {
    let dir = format!("{sub}/");
    let lower_bound = |s: &Shard, key: &str| {
        let (mut lo, mut hi) = (0usize, s.ndocs());
        while lo < hi {
            let mid = (lo + hi) / 2;
            match s.doc(mid) {
                Some(d) if d.rel < key => lo = mid + 1,
                _ => hi = mid,
            }
        }
        lo
    };
    shards.iter().any(|s| {
        s.doc(lower_bound(s, sub)).is_some_and(|d| d.rel == sub)
            || s.doc(lower_bound(s, &dir))
                .is_some_and(|d| d.rel.starts_with(&dir))
    })
}

/// Bound on remembered sub-root coverage decisions.
const COVER_CACHE_MAX: usize = 4096;

impl Engine {
    /// Open the index directory. `want_writer` tries to take the writer lock;
    /// if another process holds it, the engine opens read-only.
    pub fn open(cfg: Config, want_writer: bool) -> std::io::Result<Engine> {
        let dir = cfg.index_dir();
        std::fs::create_dir_all(&dir)?;
        let mut lock = None;
        if want_writer {
            let f = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(dir.join("writer.lock"))?;
            if f.try_lock().is_ok() {
                lock = Some(f);
            }
        }
        let e = Engine {
            cfg,
            dir,
            inner: RwLock::new(Inner {
                m: Manifest::default(),
                shards: HashMap::new(),
            }),
            build_lock: Mutex::new(()),
            writer: lock.is_some(),
            _lock: lock,
            manifest_stamp: Mutex::new(None),
            watched_dirs: Mutex::new(HashSet::new()),
            need_unit_scan: AtomicBool::new(false),
            doc_sets: Mutex::new(HashMap::new()),
            last_gen: AtomicU64::new(now_ms()),
            build_stall_ms: AtomicU64::new(0),
            shard_target: AtomicU64::new(SHARD_TARGET),
            piece_flush: AtomicU64::new(0),
            racy_ns: AtomicU64::new(RACY_NS as u64),
            anchors: OnceLock::new(),
            running: (Mutex::new(0), std::sync::Condvar::new()),
            splits: OnceLock::new(),
            reach_cache: Mutex::new(HashMap::new()),
        };
        e.reload_manifest(true);
        if e.writer {
            e.cleanup_orphans();
            let mut g = e.inner.write().unwrap();
            for u in g.m.units.values_mut() {
                // Readiness is re-earned by a listing after every start.
                u.ready = false;
                u.dirty = false;
                u.watched = false;
            }
        }
        Ok(e)
    }

    /// A changed file the overlay may serve: a regular file (never a
    /// symlink, which could point outside the corpus) within the size cap.
    fn overlay_file_ok(&self, abs: &Path) -> bool {
        std::fs::symlink_metadata(abs)
            .map(|m| m.is_file() && m.len() <= self.cfg.max_file_size)
            .unwrap_or(false)
    }

    /// Configured roots and split roots, as written and canonicalised.
    fn anchors(&self) -> &[(PathBuf, PathBuf)] {
        self.anchors.get_or_init(|| {
            let mut v = Vec::new();
            for r in self
                .cfg
                .root_paths()
                .into_iter()
                .chain(self.cfg.split_paths())
            {
                let canon = std::fs::canonicalize(&r).unwrap_or_else(|_| r.clone());
                v.push((r, canon));
            }
            v
        })
    }

    /// Is any part of `root` (below the configured root that contains it, or
    /// the whole path when none does) a secret location such as `.ssh` or
    /// `.config`? Exclusions are about the absolute location, so starting a
    /// walk inside an excluded directory does not escape them.
    pub fn secret_blocked(&self, root: &Path) -> bool {
        let set = crate::config::secret_components();
        let canon = std::fs::canonicalize(root).ok();
        for p in std::iter::once(root.to_path_buf()).chain(canon) {
            let below = self
                .anchors()
                .iter()
                .filter_map(|(a, c)| p.strip_prefix(a).or_else(|_| p.strip_prefix(c)).ok())
                .min_by_key(|r| r.components().count())
                .unwrap_or(p.as_path());
            for c in below.components() {
                if let std::path::Component::Normal(n) = c {
                    if set.is_match(n) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Check a requested search root. Secret locations are always refused.
    /// With `confined` (the HTTP API), the root must also lie inside a
    /// configured root once symlinks and `..` are resolved - however the
    /// path is spelled.
    pub fn authorize_root(&self, root: &Path, confined: bool) -> Result<(), String> {
        let lexical = crate::config::normalize(root);
        if self.secret_blocked(&lexical) || self.secret_blocked(root) {
            return Err("path is excluded (secret location)".into());
        }
        if !confined {
            return Ok(());
        }
        let inside = |p: &Path| self.anchors().iter().any(|(_, c)| p.starts_with(c));
        let raw = if root.is_absolute() {
            root.to_path_buf()
        } else {
            std::env::current_dir().unwrap_or_default().join(root)
        };
        let denied = || Err("root is not inside a configured root".to_string());
        // Both the literal spelling (symlinks and `..` resolved by the OS)
        // and the lexically normalised one must resolve inside.
        for p in [raw, lexical] {
            match std::fs::canonicalize(&p) {
                Ok(c) if inside(&c) => {}
                _ => return denied(),
            }
        }
        Ok(())
    }

    /// Wait for one of `max_concurrent_queries` slots. A server holds one
    /// while it runs a query and builds its reply, so at most that many
    /// answers are in memory at once; further requests queue (back-pressure)
    /// instead of piling up results.
    pub fn query_slot(&self) -> QuerySlot<'_> {
        let limit = self.cfg.max_concurrent_queries.max(1);
        let (m, cv) = &self.running;
        let mut n = m.lock().unwrap();
        while *n >= limit {
            n = cv.wait(n).unwrap();
        }
        *n += 1;
        QuerySlot(self)
    }

    pub fn is_writer(&self) -> bool {
        self.writer
    }

    pub fn index_dir(&self) -> &Path {
        &self.dir
    }

    fn manifest_path(&self) -> PathBuf {
        self.dir.join("manifest.json")
    }

    /// Reader side: pick up a manifest the writer replaced.
    pub fn refresh(&self) {
        if !self.writer {
            self.reload_manifest(false);
        }
    }

    fn reload_manifest(&self, force: bool) {
        let p = self.manifest_path();
        let stamp = std::fs::metadata(&p).ok().and_then(|m| {
            #[cfg(unix)]
            let ino = std::os::unix::fs::MetadataExt::ino(&m);
            #[cfg(not(unix))]
            let ino = 0;
            Some((m.modified().ok()?, m.len(), ino))
        });
        {
            let mut s = self.manifest_stamp.lock().unwrap();
            if !force && *s == stamp {
                return;
            }
            *s = stamp;
        }
        let m: Manifest = std::fs::read(&p)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let mut g = self.inner.write().unwrap();
        let mut shards = HashMap::new();
        let mut broken: Vec<String> = Vec::new();
        for (k, u) in &m.units {
            let same =
                g.m.units
                    .get(k)
                    .map(|old| old.shards == u.shards)
                    .unwrap_or(false);
            if same {
                if let Some(s) = g.shards.get(k) {
                    shards.insert(k.clone(), s.clone());
                    continue;
                }
            }
            let mut v = Vec::new();
            for name in &u.shards {
                if let Ok(s) = Shard::open(&self.dir.join(name)) {
                    v.push(s);
                }
            }
            if v.len() != u.shards.len() {
                // Missing or incompatible (older format) shards: the unit
                // must be rebuilt before it can answer.
                broken.push(k.clone());
            }
            shards.insert(k.clone(), Arc::new(v));
        }
        let mut m = m;
        for k in broken {
            if let Some(u) = m.units.get_mut(&k) {
                u.fingerprint = 0;
                u.ready = false;
            }
        }
        g.m = m;
        g.shards = shards;
    }

    fn save_manifest(&self) {
        if !self.writer {
            return;
        }
        let bytes = {
            let mut g = self.inner.write().unwrap();
            g.m.version = 1;
            g.m.heartbeat_ms = now_ms();
            g.m.rescan_secs = self.cfg.rescan_secs;
            serde_json::to_vec(&g.m).unwrap_or_default()
        };
        let tmp = self.dir.join("manifest.json.tmp");
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(&tmp, self.manifest_path());
        }
    }

    pub fn heartbeat(&self) {
        self.save_manifest();
    }

    fn cleanup_orphans(&self) {
        let keep: HashSet<String> = {
            let g = self.inner.read().unwrap();
            g.m.units
                .values()
                .flat_map(|u| u.shards.iter().cloned())
                .collect()
        };
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if (name.ends_with(".shard") || name.ends_with(".tmp")) && !keep.contains(&name) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }

    fn is_split(&self, p: &Path) -> bool {
        self.splits
            .get_or_init(|| self.cfg.split_paths())
            .iter()
            .any(|s| s == p)
    }

    /// Is `unit` a split root? Its unit holds only the files directly inside
    /// it (each child directory is a unit of its own).
    pub fn is_flat_unit(&self, unit: &str) -> bool {
        self.is_split(Path::new(unit))
    }

    /// Units that should exist right now, from the configured roots.
    pub fn discover(&self) -> Vec<String> {
        let mut out = Vec::new();
        let splits = self.cfg.split_paths();
        for root in self.cfg.root_paths() {
            if !root.is_dir() {
                continue;
            }
            // A split root is a unit too, for the files directly inside it.
            out.push(path_str(&root));
            if !splits.contains(&root) {
                continue;
            }
            if let Ok(rd) = std::fs::read_dir(&root) {
                for e in rd.flatten() {
                    let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    let name = e.file_name().to_string_lossy().into_owned();
                    if is_dir && !name.starts_with('.') {
                        out.push(path_str(&root.join(name)));
                    }
                }
            }
        }
        out.sort();
        out
    }

    /// Add new units, drop vanished ones. Returns the units added.
    pub fn sync_units(&self) -> Vec<String> {
        let want: Vec<String> = self.discover();
        let want_set: HashSet<&String> = want.iter().collect();
        let mut added = Vec::new();
        let mut dropped_shards = Vec::new();
        {
            let mut g = self.inner.write().unwrap();
            for u in &want {
                if !g.m.units.contains_key(u) {
                    g.m.units.insert(
                        u.clone(),
                        UnitState {
                            path: u.clone(),
                            ..Default::default()
                        },
                    );
                    added.push(u.clone());
                }
            }
            let gone: Vec<String> =
                g.m.units
                    .keys()
                    .filter(|k| !want_set.contains(k))
                    .cloned()
                    .collect();
            for k in gone {
                if let Some(u) = g.m.units.remove(&k) {
                    dropped_shards.extend(u.shards);
                }
                g.shards.remove(&k);
            }
        }
        if !dropped_shards.is_empty() {
            for s in dropped_shards {
                let _ = std::fs::remove_file(self.dir.join(s));
            }
            self.save_manifest();
        }
        added
    }

    pub fn unit_paths(&self) -> Vec<String> {
        self.inner.read().unwrap().m.units.keys().cloned().collect()
    }

    /// Longest unit containing `p` (or equal to it). A split root's own unit
    /// holds only its loose files, so it contains `p` only when `p` is the
    /// split root or a file (not a directory) directly inside it.
    fn unit_for(&self, g: &Inner, p: &Path) -> Option<String> {
        let mut cur = Some(p);
        while let Some(c) = cur {
            let s = path_str(c);
            if g.m.units.contains_key(&s) {
                if c != p && self.is_split(c) && (p.parent() != Some(c) || p.is_dir()) {
                    return None;
                }
                return Some(s);
            }
            cur = c.parent();
        }
        None
    }

    pub fn unit_of_path(&self, p: &Path) -> Option<String> {
        let g = self.inner.read().unwrap();
        self.unit_for(&g, p)
    }

    /// Is `rel` an indexed document of `unit`?
    pub fn is_indexed(&self, unit: &str, rel: &str) -> bool {
        let set = {
            let mut cache = self.doc_sets.lock().unwrap();
            if let Some(s) = cache.get(unit) {
                s.clone()
            } else {
                let g = self.inner.read().unwrap();
                let set: HashSet<String> = g
                    .shards
                    .get(unit)
                    .map(|v| {
                        v.iter()
                            .flat_map(|s| s.docs().map(|d| d.rel.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let set = Arc::new(set);
                cache.insert(unit.to_string(), set.clone());
                set
            }
        };
        set.contains(rel)
    }

    pub fn mark_dirty(&self, unit: &str, change: Change) {
        let mut g = self.inner.write().unwrap();
        if let Some(u) = g.m.units.get_mut(unit) {
            let t = now_ms();
            if !u.dirty {
                u.dirty = true;
                u.dirty_since_ms = t;
            }
            u.last_event_ms = t;
            if u.building {
                if u.build_dirty_since_ms == 0 {
                    u.build_dirty_since_ms = t;
                }
                match &change {
                    Change::File(rel) if u.build_pending.len() < MAX_PENDING => {
                        u.build_pending.insert(rel.clone());
                    }
                    _ => u.build_unknown = true,
                }
            }
            match change {
                Change::File(rel) if u.pending.len() < MAX_PENDING => {
                    u.pending.insert(rel);
                }
                _ => u.pending_unknown = true,
            }
        }
    }

    /// Is every change to `unit` already "unknown" (it will be re-listed
    /// whole, and a running rebuild will be followed by another)? Then a
    /// further event can only move its quiet period, and the watcher need not
    /// work out what the event changed.
    pub fn unknown_dirty(&self, unit: &str) -> bool {
        let g = self.inner.read().unwrap();
        g.m.units
            .get(unit)
            .is_some_and(|u| u.dirty && u.pending_unknown && (!u.building || u.build_unknown))
    }

    /// Persist dirty/pending state for reader processes.
    pub fn publish_state(&self) {
        self.save_manifest();
    }

    pub fn mark_all_dirty(&self) {
        let mut g = self.inner.write().unwrap();
        let t = now_ms();
        for u in g.m.units.values_mut() {
            if !u.dirty {
                u.dirty = true;
                u.dirty_since_ms = t;
            }
            u.last_event_ms = t;
            u.pending_unknown = true;
            if u.building {
                if u.build_dirty_since_ms == 0 {
                    u.build_dirty_since_ms = t;
                }
                u.build_unknown = true;
            }
        }
    }

    pub fn set_watched(&self, unit: &str, ok: bool) {
        if let Some(u) = self.inner.write().unwrap().m.units.get_mut(unit) {
            u.watched = ok;
        }
    }

    /// Units whose debounce window has passed.
    pub fn due_units(&self) -> Vec<String> {
        let g = self.inner.read().unwrap();
        let t = now_ms();
        g.m.units
            .values()
            .filter(|u| {
                u.dirty
                    && t >= u.retry_at_ms
                    && (t.saturating_sub(u.last_event_ms) >= self.cfg.debounce_ms
                        || t.saturating_sub(u.dirty_since_ms) >= self.cfg.max_wait_ms)
            })
            .map(|u| u.path.clone())
            .collect()
    }

    /// (Re)index one unit if its listing changed (or `force`). Returns the
    /// listing so the caller can (re)attach watches.
    pub fn build_unit(&self, unit: &str, force: bool) -> Option<Vec<FileEntry>> {
        if !self.writer {
            return None;
        }
        let _guard = self.build_lock.lock().unwrap();
        let prev_fp;
        let prev_shards;
        let pending_at_start: std::collections::BTreeSet<String>;
        let (dirty_age_ms, quiet_ms);
        {
            let mut g = self.inner.write().unwrap();
            let u = g.m.units.get_mut(unit)?;
            // The dirty flag and pending overlay stay as they are until the
            // new shards are published: until then queries are served from
            // the old index, which only the overlay makes exact (issue #6).
            // Changes arriving meanwhile are also tracked separately, as the
            // listing below may predate them.
            u.building = true;
            u.build_pending.clear();
            u.build_unknown = false;
            u.build_dirty_since_ms = 0;
            prev_fp = u.fingerprint;
            dirty_age_ms = if u.dirty {
                now_ms().saturating_sub(u.dirty_since_ms)
            } else {
                0
            };
            quiet_ms = now_ms().saturating_sub(u.last_event_ms);
            prev_shards = u.shards.clone();
            pending_at_start = u.pending.clone();
        }
        // A failed rebuild keeps the old index, keeps every change that was
        // pending, and marks the unit not fresh until a later build succeeds.
        let fail = |why: String, written: &[String]| -> Option<Vec<FileEntry>> {
            for n in written {
                let _ = std::fs::remove_file(self.dir.join(n));
            }
            let t = now_ms();
            {
                let mut g = self.inner.write().unwrap();
                if let Some(u) = g.m.units.get_mut(unit) {
                    u.error = Some(why);
                    // Everything pending before and during the build still is.
                    u.building = false;
                    u.build_pending.clear();
                    u.build_unknown = false;
                    u.build_dirty_since_ms = 0;
                    u.dirty = true;
                    if u.dirty_since_ms == 0 {
                        u.dirty_since_ms = t;
                    }
                    u.last_event_ms = t;
                    // The new listing was never indexed: unknown changes.
                    u.pending_unknown = true;
                    u.failures = u.failures.saturating_add(1);
                    let backoff = 1000u64 << u.failures.min(6);
                    u.retry_at_ms = t + backoff.min(60_000);
                }
            }
            self.save_manifest();
            None
        };
        let t0 = Instant::now();
        let root = PathBuf::from(unit);
        let files = if self.is_flat_unit(unit) {
            let uncovered = walk::non_unit_children(&self.cfg, &root);
            let mut g = self.inner.write().unwrap();
            if let Some(u) = g.m.units.get_mut(unit) {
                u.uncovered = uncovered;
            }
            drop(g);
            walk::list_depth(&self.cfg, &root, Some(1))
        } else {
            walk::list(&self.cfg, &root)
        };
        let t_list = t0.elapsed();
        let stall = self.build_stall_ms.load(Ordering::Relaxed);
        if stall > 0 {
            std::thread::sleep(Duration::from_millis(stall));
        }
        let fp = walk::fingerprint(&files);
        let shards_ok = prev_shards.iter().all(|s| self.dir.join(s).exists());
        // An unchanged listing means nothing to do, unless an event reported
        // a file as changed: a rewrite can keep the size and the mtime (a
        // coarse clock, a restored mtime; Windows has no ctime to tell).
        if !force
            && fp == prev_fp
            && pending_at_start.is_empty()
            && shards_ok
            && (!prev_shards.is_empty() || files.is_empty())
        {
            let mut g = self.inner.write().unwrap();
            if let Some(u) = g.m.units.get_mut(unit) {
                finish_build(u);
                u.ready = true;
                u.indexed_at_ms = now_ms();
                u.error = None;
                u.failures = 0;
            }
            return Some(files);
        }

        let gen = self.next_generation();
        let prefix = unit_prefix(unit);
        // Incremental unless forced: documents whose path, size and mtime are
        // unchanged since the last build (and that no event reported as
        // changed) keep their postings; only the rest of the listing is read.
        let old: Option<Arc<Vec<Shard>>> = if force {
            None
        } else {
            let g = self.inner.read().unwrap();
            g.shards
                .get(unit)
                .filter(|v| v.len() == prev_shards.len() && !v.is_empty())
                .cloned()
        };
        let plan = match &old {
            Some(o) => incremental_plan(o, &files, &pending_at_start),
            None => Plan {
                alive: vec![],
                alive_docs: 0,
                alive_bytes: 0,
                reindex: (0..files.len()).collect(),
            },
        };
        let mut written: Vec<String> = Vec::new();
        let built = self.build_pieces(&root, &files, &plan.reindex, &prefix, gen, &mut written);
        let (pieces, added_docs, added_bytes) = match built {
            Ok(x) => x,
            Err(e) => return fail(e, &written),
        };
        let t_pieces = t0.elapsed();
        let names = match self.compose(
            old.as_deref().map(|v| v.as_slice()).unwrap_or(&[]),
            &plan.alive,
            &pieces,
            &prefix,
            gen,
            &mut written,
        ) {
            Ok(n) => n,
            Err(e) => {
                // Unmap before removing (Windows refuses to delete a mapped file).
                drop(pieces);
                return fail(e, &written);
            }
        };
        let npieces = pieces.len();
        drop(pieces);
        if std::env::var_os("UNUMSEARCH_DEBUG_BUILD").is_some() {
            eprintln!(
                "build {unit}: dirty for {dirty_age_ms} ms, quiet {quiet_ms} ms; listed {} in {t_list:?}, reindex {} pieces {} at {:?}, composed {} at {:?}",
                files.len(),
                plan.reindex.len(),
                npieces,
                t_pieces,
                names.len(),
                t0.elapsed()
            );
        }
        // Intermediate pieces folded into the final shards.
        for n in &written {
            if !names.contains(n) {
                let _ = std::fs::remove_file(self.dir.join(n));
            }
        }
        let mut opened = Vec::new();
        for n in &names {
            match Shard::open(&self.dir.join(n)) {
                Ok(s) => opened.push(s),
                Err(e) => return fail(format!("new shard {n} unreadable: {e}"), &written),
            }
        }
        let (docs, total) = (plan.alive_docs + added_docs, plan.alive_bytes + added_bytes);
        drop(old);
        self.doc_sets.lock().unwrap().remove(unit);
        {
            let mut g = self.inner.write().unwrap();
            g.shards.insert(unit.to_string(), Arc::new(opened));
            if let Some(u) = g.m.units.get_mut(unit) {
                // Same lock as the shard swap: no query sees the new index
                // with the old overlay cleared, or the old index without it.
                finish_build(u);
                u.shards = names.clone();
                u.fingerprint = fp;
                u.files = docs;
                u.bytes = total;
                u.binary_skipped = files.len().saturating_sub(docs);
                u.indexed_at_ms = now_ms();
                u.build_ms = t0.elapsed().as_millis() as u64;
                u.ready = true;
                u.error = None;
                u.failures = 0;
                u.retry_at_ms = 0;
            }
        }
        // The manifest that references the new shards is on disk before any
        // old shard is removed, and a shard the manifest references is never
        // removed (names are unique per build, but belt and braces).
        self.save_manifest();
        release_memory();
        let keep: HashSet<&String> = names.iter().collect();
        for s in prev_shards {
            if keep.contains(&s) {
                continue;
            }
            // Readers holding the old mmap keep the inode alive (POSIX); on
            // platforms that refuse, orphan cleanup at next start removes it.
            let _ = std::fs::remove_file(self.dir.join(s));
        }
        Some(files)
    }

    /// Threads a rebuild may use: the configured count, at most
    /// [`MAX_BUILD_THREADS`], and at most one per [`BUILD_THREAD_MB`] of the
    /// memory budget.
    fn build_threads(&self) -> usize {
        self.cfg
            .thread_count()
            .min(MAX_BUILD_THREADS)
            .min((self.cfg.max_memory_mb / BUILD_THREAD_MB) as usize)
            .max(1)
    }

    /// Index `files[idx]` (in listing order) into new shard files ("pieces"),
    /// on up to `thread_count()` threads. Each thread claims consecutive
    /// blocks in increasing order, so each of its pieces is in path order.
    /// The memory budget is shared by the threads. Returns the opened pieces,
    /// and how many documents (and content bytes) they hold.
    fn build_pieces(
        &self,
        root: &Path,
        files: &[FileEntry],
        idx: &[usize],
        prefix: &str,
        gen: u64,
        written: &mut Vec<String>,
    ) -> Result<(Vec<Shard>, usize, u64), String> {
        if idx.is_empty() {
            return Ok((vec![], 0, 0));
        }
        const BLOCK: usize = 64;
        let threads = self.build_threads().min(idx.len().div_ceil(BLOCK));
        let budget = (self.cfg.max_memory_mb.max(16) as usize) << 20;
        // Postings are roughly a third of a builder's footprint at flush time.
        let flush_at = match self.piece_flush.load(Ordering::Relaxed) {
            0 => budget / 3 / threads,
            n => n as usize,
        };
        let racy_ns = self.racy_ns.load(Ordering::Relaxed) as i64;
        let next = AtomicUsize::new(0);
        let seq = AtomicUsize::new(0);
        let failed: Mutex<Option<String>> = Mutex::new(None);
        let out: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let (ndocs, nbytes) = (AtomicUsize::new(0), AtomicU64::new(0));
        let work = || {
            let mut b = ShardBuilder::new();
            let flush = |b: ShardBuilder| -> bool {
                let name = format!(
                    "{prefix}-{gen}-p{}.shard",
                    seq.fetch_add(1, Ordering::Relaxed)
                );
                out.lock().unwrap().push(name.clone());
                if let Err(e) = b.write(&self.dir.join(&name)) {
                    *failed.lock().unwrap() = Some(e.to_string());
                    return false;
                }
                true
            };
            'claim: loop {
                if failed.lock().unwrap().is_some() {
                    return;
                }
                let start = next.fetch_add(BLOCK, Ordering::Relaxed);
                if start >= idx.len() {
                    break;
                }
                for &i in &idx[start..(start + BLOCK).min(idx.len())] {
                    let f = &files[i];
                    let read_at = now_ms() as i64 * 1_000_000;
                    let content = match std::fs::read(root.join(&f.rel)) {
                        Ok(c) => c,
                        Err(_) => continue,
                    };
                    // ripgrep's default: a NUL byte marks a binary file, not searched.
                    if memchr0(&content) {
                        continue;
                    }
                    ndocs.fetch_add(1, Ordering::Relaxed);
                    nbytes.fetch_add(content.len() as u64, Ordering::Relaxed);
                    // "Racily clean" (as git calls it): stamped so close to
                    // the read that a later write could keep the stamp (a
                    // coarse filesystem clock). Stored as never matching, so
                    // the next rebuild reads it again.
                    let racy = f.mtime_ns > read_at - racy_ns;
                    b.add(
                        DocMeta {
                            rel: f.rel.clone(),
                            size: f.size,
                            mtime_ns: if racy { -1 } else { f.mtime_ns },
                        },
                        &content,
                    );
                    if b.memory() >= flush_at && !flush(std::mem::take(&mut b)) {
                        break 'claim;
                    }
                }
            }
            if !b.is_empty() {
                flush(b);
            }
        };
        if threads == 1 {
            work();
        } else {
            std::thread::scope(|sc| {
                for _ in 0..threads {
                    sc.spawn(work);
                }
            });
        }
        let names = out.into_inner().unwrap();
        written.extend(names.iter().cloned());
        if let Some(e) = failed.into_inner().unwrap() {
            return Err(e);
        }
        let mut pieces = Vec::with_capacity(names.len());
        for n in &names {
            pieces.push(
                Shard::open(&self.dir.join(n))
                    .map_err(|e| format!("new shard {n} unreadable: {e}"))?,
            );
        }
        Ok((pieces, ndocs.into_inner(), nbytes.into_inner()))
    }

    /// The unit's new shard list: old shards (keeping only their `alive`
    /// documents) plus new pieces, combined by [`shard::merge`] into shards
    /// of about [`SHARD_TARGET`]. An old shard with nothing dropped is kept as
    /// it is (same file) unless there are too many small ones; everything
    /// else is merged in groups, the groups in parallel. No source file is
    /// read here.
    fn compose(
        &self,
        old: &[Shard],
        alive: &[Vec<bool>],
        pieces: &[Shard],
        prefix: &str,
        gen: u64,
        written: &mut Vec<String>,
    ) -> Result<Vec<String>, String> {
        let file_name = |s: &Shard| {
            s.path()
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let mut names = Vec::new();
        // (input, estimated size after dropping) to merge.
        let target = self.shard_target.load(Ordering::Relaxed);
        // Shards with nothing to drop are kept as they are, unless more than
        // MAX_SMALL_SHARDS of them are under half the target: then those are
        // combined too (each such pass combines them into fewer, so this
        // settles within a few rebuilds and does not recur).
        let small_clean = old
            .iter()
            .enumerate()
            .filter(|(k, s)| {
                (s.disk_bytes() as u64) < target / 2
                    && alive.get(*k).is_none_or(|a| a.iter().all(|x| *x))
            })
            .count();
        let compact = small_clean > MAX_SMALL_SHARDS;
        let mut todo: Vec<(crate::shard::MergeInput<'_>, u64)> = Vec::new();
        for (k, s) in old.iter().enumerate() {
            let a = alive.get(k).map(|v| v.as_slice());
            let kept = a.map_or(s.ndocs(), |a| a.iter().filter(|x| **x).count());
            if kept == 0 {
                continue;
            }
            let size = s.disk_bytes() as u64;
            if kept == s.ndocs() && (size >= target / 2 || !compact) {
                names.push(file_name(s));
                continue;
            }
            let est = size * kept as u64 / s.ndocs().max(1) as u64;
            let alive = if kept == s.ndocs() { None } else { a };
            todo.push((crate::shard::MergeInput { shard: s, alive }, est));
        }
        for p in pieces {
            todo.push((
                crate::shard::MergeInput {
                    shard: p,
                    alive: None,
                },
                p.disk_bytes() as u64,
            ));
        }
        // First-fit decreasing into groups of about SHARD_TARGET.
        todo.sort_by_key(|x| std::cmp::Reverse(x.1));
        let mut groups: Vec<(Vec<crate::shard::MergeInput<'_>>, u64)> = Vec::new();
        for (inp, est) in todo {
            match groups.iter_mut().find(|g| g.1 + est <= target) {
                Some(g) => {
                    g.0.push(inp);
                    g.1 += est;
                }
                None => groups.push((vec![inp], est)),
            }
        }
        let mut jobs = Vec::new();
        for (i, (g, _)) in groups.into_iter().enumerate() {
            // A lone shard with nothing to drop is already what a merge
            // would write.
            if g.len() == 1 && g[0].alive.is_none() {
                names.push(file_name(g[0].shard));
                continue;
            }
            jobs.push((format!("{prefix}-{gen}-{i}.shard"), g));
        }
        let threads = self.build_threads();
        let next = AtomicUsize::new(0);
        let errs: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let run = || loop {
            let j = next.fetch_add(1, Ordering::Relaxed);
            let Some((name, inputs)) = jobs.get(j) else {
                break;
            };
            if let Err(e) = crate::shard::merge(inputs, &self.dir.join(name)) {
                errs.lock().unwrap().push(e.to_string());
            }
        };
        if threads == 1 || jobs.len() <= 1 {
            run();
        } else {
            std::thread::scope(|sc| {
                for _ in 0..threads.min(jobs.len()) {
                    sc.spawn(run);
                }
            });
        }
        for (n, _) in &jobs {
            written.push(n.clone());
            names.push(n.clone());
        }
        if let Some(e) = errs.into_inner().unwrap().pop() {
            return Err(e);
        }
        names.sort();
        Ok(names)
    }

    /// Test hook: aim merged shards at `shard_target` bytes and flush each
    /// reading thread's piece at `piece_flush` bytes of builder memory, so a
    /// small corpus exercises many pieces and merge groups.
    #[doc(hidden)]
    pub fn set_build_sizes(&self, shard_target: u64, piece_flush: u64) {
        self.shard_target
            .store(shard_target.max(1), Ordering::Relaxed);
        self.piece_flush.store(piece_flush, Ordering::Relaxed);
    }

    /// Test hook: treat files as racily clean only within `ns` of being read
    /// (0: never), so a test can check that the change stamp alone catches
    /// rewrites.
    #[doc(hidden)]
    pub fn set_racy_window_ns(&self, ns: u64) {
        self.racy_ns.store(ns, Ordering::Relaxed);
    }

    /// Test hook: make every rebuild of this engine take at least `ms`
    /// longer, between its listing and the publish of its new shards.
    #[doc(hidden)]
    pub fn set_build_stall_ms(&self, ms: u64) {
        self.build_stall_ms.store(ms, Ordering::Relaxed);
    }

    /// A shard generation number: strictly increasing within this process
    /// and (being seeded from the clock) across restarts, so two builds of a
    /// unit can never produce the same file name.
    fn next_generation(&self) -> u64 {
        let now = now_ms();
        let mut cur = self.last_gen.load(Ordering::Relaxed);
        loop {
            let next = now.max(cur + 1);
            match self
                .last_gen
                .compare_exchange(cur, next, Ordering::SeqCst, Ordering::Relaxed)
            {
                Ok(_) => return next,
                Err(c) => cur = c,
            }
        }
    }

    /// Bring every unit up to date. Returns (unit, listing) pairs.
    pub fn index_all(&self, force: bool) -> Vec<(String, Vec<FileEntry>)> {
        self.sync_units();
        let mut out = Vec::new();
        for u in self.unit_paths() {
            if let Some(files) = self.build_unit(&u, force) {
                out.push((u, files));
            }
        }
        self.save_manifest();
        out
    }

    fn cover(&self, g: &Inner, root: &Path) -> Option<Cover> {
        // A split root is exactly the union of its child units and its own
        // unit (the files directly inside it).
        if self.is_split(root) {
            let prefix = path_str(root);
            let units =
                g.m.units
                    .keys()
                    .filter(|k| {
                        **k == prefix
                            || Path::new(k).parent().map(|p| p == root).unwrap_or(false)
                            || k.starts_with(&(prefix.clone() + "/"))
                    })
                    .cloned()
                    .collect();
            let uncovered =
                g.m.units
                    .get(&prefix)
                    .map(|u| {
                        u.uncovered
                            .iter()
                            .map(|p| Uncovered::new(Path::new(p), "not_indexed"))
                            .collect()
                    })
                    .unwrap_or_default();
            return Some(Cover {
                units,
                sub: String::new(),
                uncovered,
            });
        }
        if let Some(u) = self.unit_for(g, root) {
            let sub = walk::rel_string(Path::new(&u), root).unwrap_or_default();
            return Some(Cover {
                units: vec![u],
                sub,
                uncovered: vec![],
            });
        }
        None
    }

    /// Why a root strictly inside `unit` (at `sub`) is not covered, if it is
    /// not: it lies in a secret location, or the unit's corpus walk never
    /// reaches it (an ignored, excluded, hidden or symlinked directory on the
    /// way, or an over-size file). `has_docs` (the index holds files below
    /// `sub`) already proves the walk reaches it. Called without the lock.
    fn sub_uncovered(
        &self,
        unit: &str,
        sub: &str,
        root: &Path,
        has_docs: bool,
        fingerprint: u64,
    ) -> Option<&'static str> {
        if has_docs || sub.is_empty() {
            return None;
        }
        if self.secret_blocked(root) {
            return Some("secret");
        }
        let key = (unit.to_string(), sub.to_string());
        if let Some(&(fp, reaches)) = self.reach_cache.lock().unwrap().get(&key) {
            if fp == fingerprint {
                return (!reaches).then_some("excluded");
            }
        }
        // Nothing there: the (empty) answer is exact whatever the rules say.
        if std::fs::symlink_metadata(root).is_err() {
            return None;
        }
        let reaches = walk::reaches(&self.cfg, Path::new(unit), root);
        let mut c = self.reach_cache.lock().unwrap();
        if c.len() >= COVER_CACHE_MAX {
            c.clear();
        }
        c.insert(key, (fingerprint, reaches));
        (!reaches).then_some("excluded")
    }

    fn writer_alive(&self, m: &Manifest) -> bool {
        if self.writer {
            return true;
        }
        let window = (m.rescan_secs.max(30) * 2 + 60) * 1000;
        now_ms().saturating_sub(m.heartbeat_ms) < window
    }

    fn statuses(&self, g: &Inner, units: &[String]) -> (Vec<UnitStatus>, bool) {
        let alive = self.writer_alive(&g.m);
        let mut fresh = true;
        let mut out = Vec::new();
        for k in units {
            if let Some(u) = g.m.units.get(k) {
                let mut s = UnitStatus::from(u);
                if !alive {
                    s.watched = false;
                }
                let recent =
                    now_ms().saturating_sub(u.indexed_at_ms) < self.cfg.rescan_secs.max(30) * 2000;
                // A dirty unit is still exact when every change is a known
                // file: those files are verified directly (see `pending`).
                let exact = !s.dirty || !u.pending_unknown;
                // The writer with a watcher trusts only watched units; readers
                // and watcher-less setups accept a recent rescan.
                let tracked = s.watched || ((!self.writer || !self.cfg.watch) && alive && recent);
                if !s.ready || !exact || !tracked {
                    fresh = false;
                }
                out.push(s);
            }
        }
        (out, fresh)
    }

    fn overrides(root: &Path, globs: &[String]) -> Result<Option<Override>, String> {
        if globs.is_empty() {
            return Ok(None);
        }
        let mut b = OverrideBuilder::new(root);
        for g in globs {
            b.add(g).map_err(|e| e.to_string())?;
        }
        b.build().map(Some).map_err(|e| e.to_string())
    }

    fn glob_ok(ov: &Option<Override>, abs: &Path) -> bool {
        match ov {
            None => true,
            Some(o) => !o.matched(abs, false).is_ignore(),
        }
    }

    pub fn search(&self, o: &SearchOpts) -> Result<SearchResult, String> {
        let t0 = Instant::now();
        self.refresh();
        let root = crate::config::normalize(&o.root);
        let pattern = if o.regex {
            o.pattern.clone()
        } else {
            trigram::escape(&o.pattern)
        };
        let plan = trigram::plan(&pattern, o.case_insensitive)?;
        let re = regex::bytes::RegexBuilder::new(&pattern)
            .case_insensitive(o.case_insensitive)
            .multi_line(true)
            .build()
            .map_err(|e| e.to_string())?;
        let ov = Self::overrides(&root, &o.globs)?;

        // Snapshot what the query needs under the lock (shards are shared
        // through `Arc`), then plan and verify without holding it.
        let (srcs, sub, units, fresh, covered, mut uncovered, probe) = {
            let g = self.inner.read().unwrap();
            match self.cover(&g, &root) {
                Some(c) => {
                    let probe = self.probe_args(&g, &c);
                    let (st, fresh) = self.statuses(&g, &c.units);
                    let mut srcs: Vec<Src> = c
                        .units
                        .iter()
                        .map(|u| Src {
                            path: PathBuf::from(u),
                            shards: g.shards.get(u).cloned(),
                            pending: g
                                .m
                                .units
                                .get(u)
                                .map(|st| st.pending.iter().cloned().collect())
                                .unwrap_or_default(),
                        })
                        .collect();
                    srcs.sort_by(|a, b| a.path.cmp(&b.path));
                    (srcs, c.sub, st, fresh, true, c.uncovered, probe)
                }
                None => (
                    Vec::new(),
                    String::new(),
                    Vec::new(),
                    false,
                    false,
                    vec![],
                    None,
                ),
            }
        };
        // A root inside a unit that the unit's corpus leaves out: the index
        // has nothing there, but a scan of the root would.
        let excluded = probe.and_then(|(unit, sub, has, fp)| {
            self.sub_uncovered(&unit, &sub, &root, has, fp)
                .map(|why| Uncovered::new(&root, why))
        });
        let (srcs, sub, units, fresh, covered) = match excluded {
            Some(u) => {
                uncovered.push(u);
                (Vec::new(), String::new(), Vec::new(), false, false)
            }
            None => (srcs, sub, units, fresh, covered),
        };
        let indexed = covered;
        let covered = covered && uncovered.is_empty();

        let mut scan_list: Vec<FileEntry> = Vec::new();
        let (srcs, backend) = if indexed {
            (srcs, "index")
        } else if o.scan_fallback && root.is_dir() && !self.secret_blocked(&root) {
            scan_list = walk::list(&self.cfg, &root);
            (
                vec![Src {
                    path: root.clone(),
                    shards: None,
                    pending: vec![],
                }],
                "scan",
            )
        } else {
            if uncovered.is_empty() {
                uncovered.push(self.outside_reason(&root));
            }
            return Ok(SearchResult {
                backend: "none",
                covered: false,
                fresh: false,
                files: vec![],
                matches: vec![],
                candidates: 0,
                truncated: false,
                complete: false,
                units: vec![],
                uncovered,
                elapsed_ms: ms(t0),
            });
        };
        if backend == "scan" && uncovered.is_empty() {
            uncovered.push(self.outside_reason(&root));
        }
        let glob_ok = |unit: &Path, rel: &str| ov.is_none() || Self::glob_ok(&ov, &unit.join(rel));
        let mut cands: Vec<Cand> = Vec::new();
        for f in &scan_list {
            if glob_ok(&root, &f.rel) {
                cands.push(Cand {
                    unit: 0,
                    rel: std::borrow::Cow::Borrowed(f.rel.as_str()),
                });
            }
        }
        for (ui, src) in srcs.iter().enumerate() {
            let ui = ui as u32;
            // Files changed since the last build are verified directly,
            // whatever the (stale) index says.
            for rel in &src.pending {
                if !in_sub(&sub, rel) {
                    continue;
                }
                let abs = src.path.join(rel);
                if self.overlay_file_ok(&abs) && Self::glob_ok(&ov, &abs) {
                    cands.push(Cand {
                        unit: ui,
                        rel: std::borrow::Cow::Borrowed(rel.as_str()),
                    });
                }
            }
            let Some(shards) = &src.shards else { continue };
            for s in shards.iter() {
                let mut take = |id: usize| {
                    let Some(d) = s.doc(id) else { return };
                    if in_sub(&sub, d.rel) && glob_ok(&src.path, d.rel) {
                        cands.push(Cand {
                            unit: ui,
                            rel: std::borrow::Cow::Borrowed(d.rel),
                        });
                    }
                };
                match s.eval(&plan) {
                    Some(v) => v.into_iter().for_each(|x| take(x as usize)),
                    None => (0..s.ndocs()).for_each(&mut take),
                }
            }
        }
        cands.sort_unstable_by(|a, b| a.unit.cmp(&b.unit).then_with(|| rel_cmp(&a.rel, &b.rel)));
        cands.dedup_by(|a, b| a.unit == b.unit && a.rel == b.rel);
        let ncand = cands.len();
        let paths: Vec<&Path> = srcs.iter().map(|s| s.path.as_path()).collect();
        let fresh = fresh || backend == "scan";

        let (files, matches, truncated) = if o.candidates_only {
            let mut files = Vec::new();
            let mut bytes = 0usize;
            let mut truncated = false;
            for c in &cands {
                let p = path_str(&paths[c.unit as usize].join(&*c.rel));
                bytes += p.len() + ITEM_OVERHEAD;
                if files.len() >= o.max_files || bytes > o.max_result_bytes {
                    truncated = true;
                    break;
                }
                files.push(p);
            }
            (files, vec![], truncated)
        } else {
            self.verify(&paths, &cands, &re, o)
        };
        Ok(SearchResult {
            backend,
            covered,
            fresh,
            files,
            matches,
            candidates: ncand,
            truncated,
            complete: covered && fresh && !truncated,
            units,
            uncovered,
            elapsed_ms: ms(t0),
        })
    }

    /// For a root strictly inside one unit: what [`Engine::sub_uncovered`]
    /// needs from under the lock (unit, whether the index has files there,
    /// the unit fingerprint). `None` for a whole unit or a split root.
    fn probe_args(&self, g: &Inner, c: &Cover) -> Option<(String, String, bool, u64)> {
        if c.sub.is_empty() || c.units.len() != 1 {
            return None;
        }
        let u = &c.units[0];
        let has = g.shards.get(u).is_some_and(|v| shards_have_sub(v, &c.sub));
        let fp = g.m.units.get(u).map(|s| s.fingerprint).unwrap_or(0);
        Some((u.clone(), c.sub.clone(), has, fp))
    }

    /// Why a root no unit contains is not covered.
    fn outside_reason(&self, root: &Path) -> Uncovered {
        let why = if self.secret_blocked(root) {
            "secret"
        } else {
            "not_indexed"
        };
        Uncovered::new(root, why)
    }

    /// Verification threads for `n` candidates: the configured count, or
    /// (auto) up to 8, and up to 16 when the candidates are a large share of
    /// a big corpus (reading them dominates; small queries gain nothing from
    /// more threads but their start-up cost).
    fn verify_threads(&self, n: usize) -> usize {
        if self.cfg.threads > 0 {
            return self.cfg.threads;
        }
        let avail = std::thread::available_parallelism().map_or(2, |n| n.get());
        if n >= BIG_VERIFY {
            avail.min(16)
        } else {
            avail.min(8)
        }
    }

    /// Read candidates and keep real matches, line by line like ripgrep.
    ///
    /// A fixed pool of workers claims blocks of candidates in path order, so
    /// the files examined always form a prefix of the candidate list. Workers
    /// stop once that prefix already holds more than the caps allow (files,
    /// matches, or result bytes); the answer is then cut in path order, so it
    /// is the same however the threads were scheduled, and marked truncated.
    fn verify(
        &self,
        roots: &[&Path],
        cands: &[Cand],
        re: &regex::bytes::Regex,
        o: &SearchOpts,
    ) -> (Vec<String>, Vec<Match>, bool) {
        let n = cands.len();
        let threads = self.verify_threads(n).min(n.div_ceil(VERIFY_BLOCK)).max(1);
        let cap = self.cfg.max_file_size;
        let next = AtomicUsize::new(0);
        let stop = AtomicBool::new(false);
        let (found_files, found_matches, found_bytes) = (
            AtomicUsize::new(0),
            AtomicUsize::new(0),
            AtomicUsize::new(0),
        );
        let hits: Mutex<Vec<(usize, Hit)>> = Mutex::new(Vec::new());
        let work = || {
            let mut buf = Vec::new();
            let mut local: Vec<(usize, Hit)> = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                let start = next.fetch_add(VERIFY_BLOCK, Ordering::Relaxed);
                if start >= n {
                    break;
                }
                let (mut nf, mut nm, mut nb) = (0usize, 0usize, 0usize);
                for (i, c) in cands
                    .iter()
                    .enumerate()
                    .take((start + VERIFY_BLOCK).min(n))
                    .skip(start)
                {
                    let p = roots[c.unit as usize].join(&*c.rel);
                    if !read_capped(&p, cap, &mut buf) || memchr0(&buf) {
                        continue;
                    }
                    if o.files_only {
                        if file_has_match(re, &buf) {
                            let ps = path_str(&p);
                            nf += 1;
                            nb += ps.len() + ITEM_OVERHEAD;
                            local.push((i, Hit::File(ps)));
                        }
                        continue;
                    }
                    if !re.is_match(&buf) {
                        continue;
                    }
                    let ps = path_str(&p);
                    let mut ms = Vec::new();
                    let mut b = 0usize;
                    for (ln, line) in buf.split(|b| *b == b'\n').enumerate() {
                        let line = line.strip_suffix(b"\r").unwrap_or(line);
                        if re.is_match(line) {
                            let text =
                                String::from_utf8_lossy(&line[..line.len().min(2000)]).into_owned();
                            b += ps.len() + text.len() + ITEM_OVERHEAD;
                            ms.push(Match {
                                path: ps.clone(),
                                line: ln as u64 + 1,
                                text,
                            });
                            // More than the caps can never be returned.
                            if ms.len() > o.max_matches || b > o.max_result_bytes {
                                break;
                            }
                        }
                    }
                    if !ms.is_empty() {
                        nf += 1;
                        nm += ms.len();
                        nb += b;
                        local.push((i, Hit::Lines(ms)));
                    }
                }
                if !local.is_empty() {
                    hits.lock().unwrap().append(&mut local);
                }
                let tf = found_files.fetch_add(nf, Ordering::Relaxed) + nf;
                let tm = found_matches.fetch_add(nm, Ordering::Relaxed) + nm;
                let tb = found_bytes.fetch_add(nb, Ordering::Relaxed) + nb;
                if tf > o.max_files || tm > o.max_matches || tb > o.max_result_bytes {
                    stop.store(true, Ordering::Relaxed);
                }
            }
        };
        if threads == 1 {
            work();
        } else {
            std::thread::scope(|sc| {
                for _ in 0..threads {
                    sc.spawn(work);
                }
            });
        }
        let mut r = hits.into_inner().unwrap();
        r.sort_unstable_by_key(|(i, _)| *i);
        let mut files = Vec::new();
        let mut matches = Vec::new();
        let mut truncated = false;
        let mut bytes = 0usize;
        for (_, hit) in r {
            if files.len() >= o.max_files || matches.len() >= o.max_matches {
                truncated = true;
                break;
            }
            match hit {
                Hit::File(p) => {
                    bytes += p.len() + ITEM_OVERHEAD;
                    if bytes > o.max_result_bytes {
                        truncated = true;
                        break;
                    }
                    files.push(p);
                }
                Hit::Lines(ms) => {
                    let room = o.max_matches - matches.len();
                    let mut take = 0;
                    for m in ms.iter().take(room) {
                        let b = m.path.len() + m.text.len() + ITEM_OVERHEAD;
                        if bytes + b > o.max_result_bytes {
                            break;
                        }
                        bytes += b;
                        take += 1;
                    }
                    if take == 0 {
                        truncated = true;
                        break;
                    }
                    if take < ms.len() {
                        truncated = true;
                    }
                    files.push(ms[0].path.clone());
                    matches.extend(ms.into_iter().take(take));
                    if truncated {
                        break;
                    }
                }
            }
        }
        (files, matches, truncated)
    }

    pub fn files(&self, o: &FilesOpts) -> Result<FilesResult, String> {
        let t0 = Instant::now();
        self.refresh();
        let root = crate::config::normalize(&o.root);
        let ov = Self::overrides(&root, &o.globs)?;
        let re = match &o.regex {
            Some(r) => Some(regex::Regex::new(r).map_err(|e| e.to_string())?),
            None => None,
        };
        let keep = |abs: &Path| -> bool {
            if !Self::glob_ok(&ov, abs) {
                return false;
            }
            match &re {
                None => true,
                Some(re) => {
                    let rel = walk::rel_string(&root, abs).unwrap_or_default();
                    let base = abs
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    re.is_match(&rel) || re.is_match(&base)
                }
            }
        };
        let mut files = Vec::new();
        let mut truncated = false;
        let mut bytes = 0usize;
        // Room for one more path (both caps), or the answer is truncated.
        let mut room = |p: &str| {
            bytes += p.len() + ITEM_OVERHEAD;
            bytes <= o.max_result_bytes
        };
        let excluded = {
            let g = self.inner.read().unwrap();
            self.cover(&g, &root).and_then(|c| self.probe_args(&g, &c))
        }
        .and_then(|(unit, sub, has, fp)| {
            self.sub_uncovered(&unit, &sub, &root, has, fp)
                .map(|why| Uncovered::new(&root, why))
        });
        let mut uncovered: Vec<Uncovered> = Vec::new();
        let g = self.inner.read().unwrap();
        let cover = match excluded {
            Some(u) => {
                uncovered.push(u);
                None
            }
            None => self.cover(&g, &root),
        };
        let (backend, covered, units, fresh) = match cover {
            Some(c) => {
                uncovered.extend(c.uncovered.iter().cloned());
                let (st, fresh) = self.statuses(&g, &c.units);
                'outer: for u in &c.units {
                    let pending = g.m.units.get(u).map(|s| &s.pending);
                    // New or changed files since the last build.
                    for rel in pending.into_iter().flatten() {
                        let abs = Path::new(u).join(rel);
                        if in_sub(&c.sub, rel) && self.overlay_file_ok(&abs) && keep(&abs) {
                            let ps = path_str(&abs);
                            if files.len() >= o.max_files || !room(&ps) {
                                truncated = true;
                                break 'outer;
                            }
                            files.push(ps);
                        }
                    }
                    let Some(shards) = g.shards.get(u) else {
                        continue;
                    };
                    for s in shards.iter() {
                        for d in s.docs() {
                            if !in_sub(&c.sub, d.rel) {
                                continue;
                            }
                            // Changed since the build: the loop above already
                            // listed it (if it still qualifies), so listing it
                            // again would only use up `max_files` on a duplicate.
                            if pending.is_some_and(|p| p.contains(d.rel)) {
                                continue;
                            }
                            let abs = Path::new(u).join(d.rel);
                            if keep(&abs) {
                                let ps = path_str(&abs);
                                if files.len() >= o.max_files || !room(&ps) {
                                    truncated = true;
                                    break 'outer;
                                }
                                files.push(ps);
                            }
                        }
                    }
                }
                ("index", uncovered.is_empty(), st, fresh)
            }
            None if o.scan_fallback && root.is_dir() && !self.secret_blocked(&root) => {
                for f in walk::list(&self.cfg, &root) {
                    let abs = root.join(&f.rel);
                    if keep(&abs) {
                        let ps = path_str(&abs);
                        if files.len() >= o.max_files || !room(&ps) {
                            truncated = true;
                            break;
                        }
                        files.push(ps);
                    }
                }
                ("scan", false, vec![], true)
            }
            None => ("none", false, vec![], false),
        };
        drop(g);
        if !covered && uncovered.is_empty() {
            uncovered.push(self.outside_reason(&root));
        }
        files.sort();
        files.dedup();
        Ok(FilesResult {
            backend,
            covered,
            fresh,
            files,
            truncated,
            complete: covered && fresh && !truncated,
            units,
            uncovered,
            elapsed_ms: ms(t0),
        })
    }

    pub fn status(&self) -> serde_json::Value {
        self.refresh();
        let g = self.inner.read().unwrap();
        let units: Vec<String> = g.m.units.keys().cloned().collect();
        let (st, fresh) = self.statuses(&g, &units);
        let disk: u64 = g
            .shards
            .values()
            .flat_map(|v| v.iter())
            .map(|s| s.disk_bytes() as u64)
            .sum();
        let files: usize = g.m.units.values().map(|u| u.files).sum();
        let bytes: u64 = g.m.units.values().map(|u| u.bytes).sum();
        let ready = g.m.units.values().filter(|u| u.ready).count();
        let dirty = g.m.units.values().filter(|u| u.dirty).count();
        serde_json::json!({
            "ok": true,
            "version": env!("CARGO_PKG_VERSION"),
            "index_dir": path_str(&self.dir),
            "writer": self.writer,
            "writer_alive": self.writer_alive(&g.m),
            "heartbeat_ms": g.m.heartbeat_ms,
            "fresh": fresh,
            "units_total": st.len(),
            "units_ready": ready,
            "units_dirty": dirty,
            "files": files,
            "content_bytes": bytes,
            "index_bytes": disk,
            "rss_bytes": rss_bytes(),
            "units": st,
        })
    }
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn memchr0(b: &[u8]) -> bool {
    b.contains(&0)
}

/// Resident set size of this process, where the platform makes it cheap.
pub fn rss_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let s = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = s.split_whitespace().nth(1)?.parse().ok()?;
        Some(pages * 4096)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Is `rel` (a unit-relative path) inside the sub-directory `sub` ("" = all)?
fn in_sub(sub: &str, rel: &str) -> bool {
    sub.is_empty()
        || rel == sub
        || (rel.len() > sub.len() && rel.starts_with(sub) && rel.as_bytes()[sub.len()] == b'/')
}

/// Return freed heap to the OS after a build. Index building allocates in
/// bursts; glibc otherwise keeps the peak resident indefinitely.
pub fn release_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        extern "C" {
            fn malloc_trim(pad: usize) -> i32;
        }
        // SAFETY: malloc_trim has no preconditions.
        unsafe {
            malloc_trim(0);
        }
    }
}

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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// What a filesystem event changed in a unit.
#[derive(Clone, Debug)]
pub enum Change {
    File(String),
    Unknown,
}

const MAX_PENDING: usize = 2000;

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
    manifest_stamp: Mutex<Option<SystemTime>>,
    pub(crate) watched_dirs: Mutex<HashSet<PathBuf>>,
    pub(crate) need_unit_scan: AtomicBool,
    doc_sets: Mutex<HashMap<String, Arc<HashSet<String>>>>,
}

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
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Match {
    pub path: String,
    pub line: u64,
    pub text: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct SearchResult {
    pub backend: &'static str,
    pub covered: bool,
    pub fresh: bool,
    pub files: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub matches: Vec<Match>,
    pub candidates: usize,
    pub truncated: bool,
    pub units: Vec<UnitStatus>,
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
}

#[derive(Clone, Debug, Serialize)]
pub struct FilesResult {
    pub backend: &'static str,
    pub covered: bool,
    pub fresh: bool,
    pub files: Vec<String>,
    pub truncated: bool,
    pub units: Vec<UnitStatus>,
    pub elapsed_ms: f64,
}

struct Cover {
    units: Vec<String>,
    /// '/'-separated path of root inside the single covering unit ("" = whole unit).
    sub: String,
}

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
        let stamp = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
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
        self.cfg.split_paths().iter().any(|s| s == p)
    }

    /// Units that should exist right now, from the configured roots.
    pub fn discover(&self) -> Vec<String> {
        let mut out = Vec::new();
        let splits = self.cfg.split_paths();
        for root in self.cfg.root_paths() {
            if !root.is_dir() {
                continue;
            }
            if !splits.contains(&root) {
                out.push(path_str(&root));
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

    /// Longest unit containing `p` (or equal to it).
    fn unit_for(&self, g: &Inner, p: &Path) -> Option<String> {
        let mut cur = Some(p);
        while let Some(c) = cur {
            let s = path_str(c);
            if g.m.units.contains_key(&s) {
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
                            .flat_map(|s| s.docs.iter().map(|d| d.rel.clone()))
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
            match change {
                Change::File(rel) if u.pending.len() < MAX_PENDING => {
                    u.pending.insert(rel);
                }
                _ => u.pending_unknown = true,
            }
        }
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
        {
            let mut g = self.inner.write().unwrap();
            let u = g.m.units.get_mut(unit)?;
            // Cleared before listing: events during the build re-mark it.
            u.dirty = false;
            u.pending.clear();
            u.pending_unknown = false;
            prev_fp = u.fingerprint;
            prev_shards = u.shards.clone();
        }
        let t0 = Instant::now();
        let root = PathBuf::from(unit);
        let files = walk::list(&self.cfg, &root);
        let fp = walk::fingerprint(&files);
        let shards_ok = prev_shards.iter().all(|s| self.dir.join(s).exists());
        if !force && fp == prev_fp && shards_ok && (!prev_shards.is_empty() || files.is_empty()) {
            let mut g = self.inner.write().unwrap();
            if let Some(u) = g.m.units.get_mut(unit) {
                u.ready = true;
                u.indexed_at_ms = now_ms();
                u.error = None;
            }
            return Some(files);
        }

        let budget = (self.cfg.max_memory_mb.max(16) as usize) << 20;
        // Postings are roughly a third of the builder's footprint at flush time.
        let flush_at = budget / 3;
        let gen = now_ms();
        let prefix = unit_prefix(unit);
        let mut names = Vec::new();
        let mut b = ShardBuilder::new();
        let mut total = 0u64;
        let mut binary = 0usize;
        let mut docs = 0usize;
        let mut err: Option<String> = None;
        for f in &files {
            let content = match std::fs::read(root.join(&f.rel)) {
                Ok(c) => c,
                Err(_) => continue,
            };
            // ripgrep's default: a NUL byte marks a binary file, not searched.
            if memchr0(&content) {
                binary += 1;
                continue;
            }
            total += content.len() as u64;
            docs += 1;
            b.add(
                DocMeta {
                    rel: f.rel.clone(),
                    size: f.size,
                    mtime_ns: f.mtime_ns,
                },
                &content,
            );
            if b.memory() >= flush_at {
                let name = format!("{prefix}-{gen}-{}.shard", names.len());
                if let Err(e) = std::mem::take(&mut b).write(&self.dir.join(&name)) {
                    err = Some(e.to_string());
                    break;
                }
                names.push(name);
            }
        }
        if err.is_none() && !b.is_empty() {
            let name = format!("{prefix}-{gen}-{}.shard", names.len());
            match b.write(&self.dir.join(&name)) {
                Ok(()) => names.push(name),
                Err(e) => err = Some(e.to_string()),
            }
        }
        if let Some(e) = err {
            for n in &names {
                let _ = std::fs::remove_file(self.dir.join(n));
            }
            if let Some(u) = self.inner.write().unwrap().m.units.get_mut(unit) {
                u.error = Some(e);
            }
            return None;
        }
        let mut opened = Vec::new();
        for n in &names {
            if let Ok(s) = Shard::open(&self.dir.join(n)) {
                opened.push(s);
            }
        }
        self.doc_sets.lock().unwrap().remove(unit);
        {
            let mut g = self.inner.write().unwrap();
            g.shards.insert(unit.to_string(), Arc::new(opened));
            if let Some(u) = g.m.units.get_mut(unit) {
                u.shards = names;
                u.fingerprint = fp;
                u.files = docs;
                u.bytes = total;
                u.binary_skipped = binary;
                u.indexed_at_ms = now_ms();
                u.build_ms = t0.elapsed().as_millis() as u64;
                u.ready = true;
                u.error = None;
            }
        }
        self.save_manifest();
        release_memory();
        for s in prev_shards {
            // Readers holding the old mmap keep the inode alive (POSIX); on
            // platforms that refuse, orphan cleanup at next start removes it.
            let _ = std::fs::remove_file(self.dir.join(s));
        }
        Some(files)
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
        if let Some(u) = self.unit_for(g, root) {
            let sub = walk::rel_string(Path::new(&u), root).unwrap_or_default();
            return Some(Cover {
                units: vec![u],
                sub,
            });
        }
        // A split root is exactly the union of its child units.
        if self.is_split(root) {
            let prefix = path_str(root);
            let units =
                g.m.units
                    .keys()
                    .filter(|k| {
                        Path::new(k).parent().map(|p| p == root).unwrap_or(false)
                            || k.starts_with(&(prefix.clone() + "/"))
                    })
                    .cloned()
                    .collect();
            return Some(Cover {
                units,
                sub: String::new(),
            });
        }
        None
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

        let (cands, units, fresh, covered) = {
            let g = self.inner.read().unwrap();
            match self.cover(&g, &root) {
                Some(c) => {
                    let (st, fresh) = self.statuses(&g, &c.units);
                    let mut cands = Vec::new();
                    for u in &c.units {
                        // Files changed since the last build are verified
                        // directly, whatever the (stale) index says.
                        if let Some(st) = g.m.units.get(u) {
                            for rel in &st.pending {
                                if !in_sub(&c.sub, rel) {
                                    continue;
                                }
                                let abs = Path::new(u).join(rel);
                                if abs.is_file() && Self::glob_ok(&ov, &abs) {
                                    cands.push(abs);
                                }
                            }
                        }
                        let Some(shards) = g.shards.get(u) else {
                            continue;
                        };
                        for s in shards.iter() {
                            let ids: Box<dyn Iterator<Item = usize>> = match s.eval(&plan) {
                                Some(v) => Box::new(v.into_iter().map(|x| x as usize)),
                                None => Box::new(0..s.docs.len()),
                            };
                            for id in ids {
                                let d = &s.docs[id];
                                if !c.sub.is_empty()
                                    && d.rel != c.sub
                                    && !d.rel.starts_with(&(c.sub.clone() + "/"))
                                {
                                    continue;
                                }
                                let abs = Path::new(u).join(&d.rel);
                                if Self::glob_ok(&ov, &abs) {
                                    cands.push(abs);
                                }
                            }
                        }
                    }
                    (cands, st, fresh, true)
                }
                None => (Vec::new(), Vec::new(), false, false),
            }
        };

        let (mut cands, backend) = if covered {
            (cands, "index")
        } else if o.scan_fallback && root.is_dir() {
            let files = walk::list(&self.cfg, &root);
            let c = files
                .into_iter()
                .map(|f| root.join(f.rel))
                .filter(|p| Self::glob_ok(&ov, p))
                .collect();
            (c, "scan")
        } else {
            return Ok(SearchResult {
                backend: "none",
                covered: false,
                fresh: false,
                files: vec![],
                matches: vec![],
                candidates: 0,
                truncated: false,
                units: vec![],
                elapsed_ms: ms(t0),
            });
        };
        cands.sort();
        cands.dedup();
        let ncand = cands.len();

        if o.candidates_only {
            let truncated = cands.len() > o.max_files;
            cands.truncate(o.max_files);
            return Ok(SearchResult {
                backend,
                covered,
                fresh: fresh || backend == "scan",
                files: cands.iter().map(|p| path_str(p)).collect(),
                matches: vec![],
                candidates: ncand,
                truncated,
                units,
                elapsed_ms: ms(t0),
            });
        }

        let (files, matches, truncated) = self.verify(&cands, &re, o);
        Ok(SearchResult {
            backend,
            covered,
            fresh: fresh || backend == "scan",
            files,
            matches,
            candidates: ncand,
            truncated,
            units,
            elapsed_ms: ms(t0),
        })
    }

    /// Read candidates and keep real matches, line by line like ripgrep.
    fn verify(
        &self,
        cands: &[PathBuf],
        re: &regex::bytes::Regex,
        o: &SearchOpts,
    ) -> (Vec<String>, Vec<Match>, bool) {
        let threads = self.cfg.thread_count();
        let mut files = Vec::new();
        let mut matches = Vec::new();
        let mut truncated = false;
        let block = 256.max(threads * 16);
        for chunk in cands.chunks(block) {
            let next = AtomicUsize::new(0);
            let results: Mutex<Vec<(usize, Vec<Match>)>> = Mutex::new(Vec::new());
            std::thread::scope(|sc| {
                for _ in 0..threads.min(chunk.len()) {
                    sc.spawn(|| loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= chunk.len() {
                            break;
                        }
                        let p = &chunk[i];
                        let Ok(buf) = std::fs::read(p) else { continue };
                        if memchr0(&buf) || !re.is_match(&buf) {
                            continue;
                        }
                        let mut ms = Vec::new();
                        for (n, line) in buf.split(|b| *b == b'\n').enumerate() {
                            let line = line.strip_suffix(b"\r").unwrap_or(line);
                            if re.is_match(line) {
                                ms.push(Match {
                                    path: path_str(p),
                                    line: n as u64 + 1,
                                    text: String::from_utf8_lossy(&line[..line.len().min(2000)])
                                        .into_owned(),
                                });
                                if o.files_only {
                                    break;
                                }
                            }
                        }
                        if !ms.is_empty() {
                            results.lock().unwrap().push((i, ms));
                        }
                    });
                }
            });
            let mut r = results.into_inner().unwrap();
            r.sort_by_key(|(i, _)| *i);
            for (_, ms) in r {
                if files.len() >= o.max_files || matches.len() >= o.max_matches {
                    truncated = true;
                    break;
                }
                files.push(ms[0].path.clone());
                if !o.files_only {
                    let room = o.max_matches - matches.len();
                    if ms.len() > room {
                        truncated = true;
                    }
                    matches.extend(ms.into_iter().take(room));
                }
            }
            if truncated {
                break;
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
        let g = self.inner.read().unwrap();
        let (backend, covered, units, fresh) = match self.cover(&g, &root) {
            Some(c) => {
                let (st, fresh) = self.statuses(&g, &c.units);
                'outer: for u in &c.units {
                    let pending = g.m.units.get(u).map(|s| &s.pending);
                    // New or changed files since the last build.
                    for rel in pending.into_iter().flatten() {
                        let abs = Path::new(u).join(rel);
                        if in_sub(&c.sub, rel) && abs.is_file() && keep(&abs) {
                            files.push(path_str(&abs));
                        }
                    }
                    let Some(shards) = g.shards.get(u) else {
                        continue;
                    };
                    for s in shards.iter() {
                        for d in &s.docs {
                            if !in_sub(&c.sub, &d.rel) {
                                continue;
                            }
                            // Deleted since the build.
                            if pending.is_some_and(|p| p.contains(&d.rel))
                                && !Path::new(u).join(&d.rel).is_file()
                            {
                                continue;
                            }
                            let abs = Path::new(u).join(&d.rel);
                            if keep(&abs) {
                                if files.len() >= o.max_files {
                                    truncated = true;
                                    break 'outer;
                                }
                                files.push(path_str(&abs));
                            }
                        }
                    }
                }
                ("index", true, st, fresh)
            }
            None if o.scan_fallback && root.is_dir() => {
                for f in walk::list(&self.cfg, &root) {
                    let abs = root.join(&f.rel);
                    if keep(&abs) {
                        if files.len() >= o.max_files {
                            truncated = true;
                            break;
                        }
                        files.push(path_str(&abs));
                    }
                }
                ("scan", false, vec![], true)
            }
            None => ("none", false, vec![], false),
        };
        files.sort();
        files.dedup();
        Ok(FilesResult {
            backend,
            covered,
            fresh,
            files,
            truncated,
            units,
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

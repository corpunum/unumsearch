// SPDX-License-Identifier: Apache-2.0
//! Keeping the index fresh: filesystem notifications + debounce + rescans.
//!
//! On Linux and other inotify/kqueue-style platforms, watches are placed only
//! on directories that hold indexed files (and their ancestors up to the unit
//! root), never on ignored trees like `node_modules/` or `target/`, so the
//! per-user watch limit is spent on the corpus alone. On macOS (FSEvents) and
//! Windows (ReadDirectoryChangesW) recursive watches are cheap, so each unit
//! root is watched recursively and excluded paths are filtered on arrival.
//!
//! Whatever the watcher misses (overflowed queues, exhausted watch limits,
//! network filesystems) the periodic rescan catches: it re-lists every unit
//! and rebuilds those whose size/mtime fingerprint moved.

use crate::engine::{Change, Engine};
use crate::walk::{self, FileEntry};
use notify::{EventKind, RecursiveMode, Watcher};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

const RECURSIVE: bool = cfg!(any(target_os = "macos", target_os = "windows"));

fn dirs_for(unit: &Path, files: &[FileEntry]) -> HashSet<PathBuf> {
    let mut dirs = HashSet::new();
    dirs.insert(unit.to_path_buf());
    for f in files {
        let mut d = unit.join(&f.rel);
        while d.pop() {
            if d.as_path() == unit || !d.starts_with(unit) || !dirs.insert(d.clone()) {
                break;
            }
        }
    }
    dirs
}

struct Ctx<W: Watcher> {
    engine: Arc<Engine>,
    watcher: Option<W>,
}

impl<W: Watcher> Ctx<W> {
    /// Watch the unit's directories; returns how many watches were added.
    fn attach(&mut self, unit: &str, files: &[FileEntry]) -> usize {
        let Some(w) = self.watcher.as_mut() else {
            return 0;
        };
        let unit_p = PathBuf::from(unit);
        let mut ok = true;
        let mut added = 0;
        let mut watched = self.engine.watched_dirs.lock().unwrap();
        if RECURSIVE {
            if !watched.contains(&unit_p) {
                ok = w.watch(&unit_p, RecursiveMode::Recursive).is_ok();
                if ok {
                    watched.insert(unit_p.clone());
                    added += 1;
                }
            }
        } else {
            for d in dirs_for(&unit_p, files) {
                if watched.contains(&d) {
                    continue;
                }
                if w.watch(&d, RecursiveMode::NonRecursive).is_ok() {
                    watched.insert(d);
                    added += 1;
                } else {
                    ok = false;
                }
            }
        }
        drop(watched);
        self.engine.set_watched(unit, ok);
        added
    }

    fn detach_missing(&mut self) {
        let units: HashSet<String> = self.engine.unit_paths().into_iter().collect();
        let Some(w) = self.watcher.as_mut() else {
            return;
        };
        let mut watched = self.engine.watched_dirs.lock().unwrap();
        let stale: Vec<PathBuf> = watched
            .iter()
            .filter(|d| {
                let mut cur = Some(d.as_path());
                while let Some(c) = cur {
                    if units.contains(&c.to_string_lossy().into_owned()) {
                        return false;
                    }
                    cur = c.parent();
                }
                true
            })
            .cloned()
            .collect();
        for d in stale {
            let _ = w.unwatch(&d);
            watched.remove(&d);
        }
    }

    fn build(&mut self, unit: &str) {
        // Changes made between a listing and the watches that cover them
        // would be missed until the next rescan, so after adding watches
        // list once more (cheap when nothing changed: fingerprint match).
        for _ in 0..3 {
            let Some(files) = self.engine.build_unit(unit, false) else {
                return;
            };
            if self.attach(unit, &files) == 0 {
                return;
            }
        }
    }

    fn rescan(&mut self) {
        let added = self.engine.sync_units();
        self.detach_missing();
        let mut all = self.engine.unit_paths();
        // New units first: they have no index at all yet.
        all.sort_by_key(|u| !added.contains(u));
        for u in all {
            self.build(&u);
        }
        self.engine.heartbeat();
        crate::engine::release_memory();
    }
}

/// Does an event on `p` change the corpus of `unit`? Writes to files that are
/// not indexed and would not be (gitignored, excluded, oversized, temp files
/// renamed away) are noise: a busy gitignored state file must not keep a unit
/// permanently dirty.
fn relevant(
    engine: &Engine,
    unit: &str,
    p: &Path,
    listings: &mut HashMap<PathBuf, HashSet<String>>,
) -> Option<Change> {
    let unit_p = Path::new(unit);
    if p == unit_p {
        return Some(Change::Unknown);
    }
    let rel = walk::rel_string(unit_p, p)?;
    if engine.is_indexed(unit, &rel) {
        return Some(Change::File(rel));
    }
    // A directory we watch appeared, vanished or was renamed.
    if engine.watched_dirs.lock().unwrap().contains(p) {
        return Some(Change::Unknown);
    }
    let (parent, name) = (p.parent()?, p.file_name()?.to_string_lossy().into_owned());
    // Ignore files change what the corpus is.
    if matches!(name.as_str(), ".gitignore" | ".ignore" | ".rgignore") {
        return Some(Change::Unknown);
    }
    let entries = listings
        .entry(parent.to_path_buf())
        .or_insert_with(|| walk::corpus_entries(&engine.cfg, unit_p, parent));
    if !entries.contains(&name) {
        return None;
    }
    // A new directory may hold any number of files: rebuild before trusting.
    if p.is_dir() {
        Some(Change::Unknown)
    } else {
        Some(Change::File(rel))
    }
}

/// Canonical-path prefixes of units that differ from their configured path.
struct Aliases(Vec<(PathBuf, PathBuf)>);

impl Aliases {
    fn build(engine: &Engine) -> Aliases {
        let mut v = Vec::new();
        let mut roots: Vec<PathBuf> = engine.unit_paths().into_iter().map(PathBuf::from).collect();
        roots.extend(engine.cfg.split_paths());
        for u in roots {
            if let Ok(c) = std::fs::canonicalize(&u) {
                if c != u {
                    v.push((c, u));
                }
            }
        }
        Aliases(v)
    }

    fn to_configured(&self, p: &Path) -> PathBuf {
        for (canon, configured) in &self.0 {
            if let Ok(rest) = p.strip_prefix(canon) {
                return configured.join(rest);
            }
        }
        p.to_path_buf()
    }
}

/// Run the writer loop until `stop` is set: initial index, then watch,
/// debounce-rebuild and periodic rescans. Blocks the calling thread.
pub fn run(engine: Arc<Engine>, stop: Arc<AtomicBool>) {
    let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
    let watcher = if engine.cfg.watch {
        match notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
        }) {
            Ok(w) => Some(w),
            Err(e) => {
                eprintln!("unumsearch: watcher unavailable ({e}); relying on rescans");
                None
            }
        }
    } else {
        None
    };
    let mut ctx = Ctx {
        engine: engine.clone(),
        watcher,
    };
    // Split roots are watched themselves so new units appear promptly.
    if let Some(w) = ctx.watcher.as_mut() {
        for s in engine.cfg.split_paths() {
            let _ = w.watch(&s, RecursiveMode::NonRecursive);
        }
    }
    let splits: Vec<PathBuf> = engine.cfg.split_paths();

    let debug = std::env::var_os("UNUMSEARCH_DEBUG_EVENTS").is_some();
    ctx.rescan();
    let rescan_every = Duration::from_secs(engine.cfg.rescan_secs.max(10));
    let mut last_rescan = Instant::now();
    let mut last_beat = Instant::now();
    let mut last_publish = Instant::now();
    let mut aliases = Aliases::build(&engine);
    let mut changed = false;

    while !stop.load(Ordering::Relaxed) {
        // Directory listings are cached for one drain cycle only.
        let mut listings: HashMap<PathBuf, HashSet<String>> = HashMap::new();
        // Drain events for up to 200 ms.
        let deadline = Instant::now() + Duration::from_millis(200);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(Ok(ev)) => {
                    if matches!(ev.kind, EventKind::Access(_)) {
                        continue;
                    }
                    if ev.need_rescan() {
                        engine.mark_all_dirty();
                        continue;
                    }
                    for raw in &ev.paths {
                        // Watchers may report canonical paths (macOS: /private/var
                        // for /var); map them back to the configured spelling.
                        let p = &aliases.to_configured(raw);
                        if let Some(parent) = p.parent() {
                            if splits.iter().any(|s| s == parent) {
                                engine.need_unit_scan.store(true, Ordering::Relaxed);
                            }
                        }
                        let Some(unit) = engine.unit_of_path(p) else {
                            continue;
                        };
                        if let Some(change) = relevant(&engine, &unit, p, &mut listings) {
                            if debug {
                                eprintln!("unumsearch: change {change:?} in {unit}");
                            }
                            engine.mark_dirty(&unit, change);
                            changed = true;
                        }
                    }
                }
                Ok(Err(_)) => {
                    // Typically a queue overflow: we no longer know what changed.
                    engine.mark_all_dirty();
                }
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    std::thread::sleep(left);
                    break;
                }
            }
        }
        // Readers (CLI, MCP) learn about pending changes from the manifest.
        if changed && last_publish.elapsed() >= Duration::from_secs(1) {
            engine.publish_state();
            last_publish = Instant::now();
            changed = false;
        }
        if engine.need_unit_scan.swap(false, Ordering::Relaxed) {
            for u in engine.sync_units() {
                ctx.build(&u);
            }
            ctx.detach_missing();
        }
        for u in engine.due_units() {
            ctx.build(&u);
        }
        if last_rescan.elapsed() >= rescan_every {
            ctx.rescan();
            aliases = Aliases::build(&engine);
            last_rescan = Instant::now();
        }
        if last_beat.elapsed() >= Duration::from_secs(30) {
            engine.heartbeat();
            last_beat = Instant::now();
        }
    }
}

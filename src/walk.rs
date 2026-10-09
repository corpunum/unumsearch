// SPDX-License-Identifier: Apache-2.0
//! The corpus: which files under a directory are searchable.
//!
//! Built on the `ignore` crate (ripgrep's own walker), so .gitignore/.ignore
//! handling, hidden-file rules and size limits mean exactly what they mean to
//! ripgrep. Excludes are applied on top as gitignore-syntax patterns.

use crate::config::Config;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

#[derive(Clone, Debug)]
pub struct FileEntry {
    /// Path relative to the walk root, '/'-separated.
    pub rel: String,
    pub size: u64,
    pub mtime_ns: i64,
}

/// Exclude matcher anchored at `root` (patterns with a slash are relative to it).
pub fn exclude_matcher(cfg: &Config, root: &Path) -> Gitignore {
    let mut b = GitignoreBuilder::new(root);
    for line in cfg.exclude_lines() {
        let _ = b.add_line(None, &line);
    }
    b.build().unwrap_or_else(|_| Gitignore::empty())
}

/// Is `path` (absolute, under `root`) excluded by configuration? Checks the
/// path and every parent below root, as a walk would have pruned them.
pub fn is_excluded(m: &Gitignore, root: &Path, path: &Path, is_dir: bool) -> bool {
    if !path.starts_with(root) {
        return false;
    }
    m.matched_path_or_any_parents(path, is_dir).is_ignore()
}

pub fn rel_string(root: &Path, p: &Path) -> Option<String> {
    let rel = p.strip_prefix(root).ok()?;
    let mut s = String::new();
    for (i, c) in rel.components().enumerate() {
        if i > 0 {
            s.push('/');
        }
        s.push_str(&c.as_os_str().to_string_lossy());
    }
    Some(s)
}

pub fn mtime_ns(md: &std::fs::Metadata) -> i64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// List the corpus under `root`, sorted by relative path.
pub fn list(cfg: &Config, root: &Path) -> Vec<FileEntry> {
    list_depth(cfg, root, None)
}

/// [`list`], descending at most `max_depth` levels (`Some(1)`: only the
/// files directly inside `root`).
pub fn list_depth(cfg: &Config, root: &Path, max_depth: Option<usize>) -> Vec<FileEntry> {
    let excl = exclude_matcher(cfg, root);
    let mut wb = WalkBuilder::new(root);
    wb.max_depth(max_depth);
    wb.hidden(!cfg.hidden)
        .git_ignore(cfg.gitignore)
        .git_global(cfg.gitignore)
        .git_exclude(cfg.gitignore)
        .ignore(cfg.gitignore)
        .parents(cfg.gitignore)
        .follow_links(false)
        .max_filesize(Some(cfg.max_file_size))
        .threads(cfg.thread_count().min(4));
    let root_owned: PathBuf = root.to_path_buf();
    let excl = std::sync::Arc::new(excl);
    wb.filter_entry(move |e| {
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        // The walk prunes top-down, so the entry itself is all we must check.
        e.path() == root_owned || !excl.matched(e.path(), is_dir).is_ignore()
    });
    let out: Mutex<Vec<FileEntry>> = Mutex::new(Vec::new());
    wb.build_parallel().run(|| {
        let out = &out;
        Box::new(move |res| {
            if let Ok(e) = res {
                if e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    if let Ok(md) = e.metadata() {
                        if let Some(rel) = rel_string(root, e.path()) {
                            out.lock().unwrap().push(FileEntry {
                                rel,
                                size: md.len(),
                                mtime_ns: mtime_ns(&md),
                            });
                        }
                    }
                }
            }
            ignore::WalkState::Continue
        })
    });
    let mut v = out.into_inner().unwrap();
    v.sort_by(|a, b| a.rel.cmp(&b.rel));
    v
}

/// Does the corpus walk of `unit` reach `target` (a path below `unit`)? It
/// does unless `target` or a directory between them is pruned by the walk's
/// rules: ignore files, hidden-file rules, excludes (secrets included),
/// symlinks (never followed) or, for a file, the size cap. Only the
/// directories on the way to `target` are read.
pub fn reaches(cfg: &Config, unit: &Path, target: &Path) -> bool {
    let Ok(rel) = target.strip_prefix(unit) else {
        return false;
    };
    let depth = rel.components().count();
    if depth == 0 {
        return true;
    }
    let excl = exclude_matcher(cfg, unit);
    let mut wb = WalkBuilder::new(unit);
    wb.max_depth(Some(depth))
        .hidden(!cfg.hidden)
        .git_ignore(cfg.gitignore)
        .git_global(cfg.gitignore)
        .git_exclude(cfg.gitignore)
        .ignore(cfg.gitignore)
        .parents(cfg.gitignore)
        .follow_links(false);
    let (unit_owned, target_owned) = (unit.to_path_buf(), target.to_path_buf());
    // Same pruning as `list`, restricted to the path towards `target`.
    wb.filter_entry(move |e| {
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        e.path() == unit_owned
            || (target_owned.starts_with(e.path()) && !excl.matched(e.path(), is_dir).is_ignore())
    });
    for e in wb.build().flatten() {
        if e.path() == target {
            // A directory is walked; a file is indexed within the size cap; a
            // symlink (or anything else) never is.
            return match e.file_type() {
                Some(t) if t.is_dir() => true,
                Some(t) if t.is_file() => e.metadata().is_ok_and(|m| m.len() <= cfg.max_file_size),
                _ => false,
            };
        }
    }
    false
}

/// Directories directly inside the split root `root` that its corpus walk
/// would descend into but that are not units of their own (hidden names):
/// a split root's index does not cover them.
pub fn non_unit_children(cfg: &Config, root: &Path) -> Vec<String> {
    let excl = std::sync::Arc::new(exclude_matcher(cfg, root));
    let mut wb = WalkBuilder::new(root);
    wb.max_depth(Some(1))
        .hidden(!cfg.hidden)
        .git_ignore(cfg.gitignore)
        .git_global(cfg.gitignore)
        .git_exclude(cfg.gitignore)
        .ignore(cfg.gitignore)
        .parents(cfg.gitignore)
        .follow_links(false);
    let root_owned = root.to_path_buf();
    wb.filter_entry(move |e| {
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        e.path() == root_owned || (is_dir && !excl.matched(e.path(), true).is_ignore())
    });
    let mut out: Vec<String> = wb
        .build()
        .flatten()
        .filter(|e| e.depth() == 1 && e.file_type().is_some_and(|t| t.is_dir()))
        .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

/// FNV-1a over the listing: changes when any path, size or mtime changes.
pub fn fingerprint(files: &[FileEntry]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut feed = |b: &[u8]| {
        for x in b {
            h ^= *x as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    };
    for f in files {
        feed(f.rel.as_bytes());
        feed(&[0]);
        feed(&f.size.to_le_bytes());
        feed(&f.mtime_ns.to_le_bytes());
    }
    h
}

/// Names of the entries directly inside `dir` (a directory under `unit`) that
/// belong to the corpus: files that `list` would return, and directories it
/// would descend into. Used to decide whether a filesystem event matters
/// without re-walking the unit.
pub fn corpus_entries(cfg: &Config, unit: &Path, dir: &Path) -> std::collections::HashSet<String> {
    let excl = std::sync::Arc::new(exclude_matcher(cfg, unit));
    let mut wb = WalkBuilder::new(dir);
    wb.hidden(!cfg.hidden)
        .git_ignore(cfg.gitignore)
        .git_global(cfg.gitignore)
        .git_exclude(cfg.gitignore)
        .ignore(cfg.gitignore)
        .parents(cfg.gitignore)
        .follow_links(false)
        .max_filesize(Some(cfg.max_file_size))
        .max_depth(Some(1));
    let dir_owned = dir.to_path_buf();
    let unit_owned = unit.to_path_buf();
    let filter_excl = excl.clone();
    wb.filter_entry(move |e| {
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        e.path() == dir_owned || !is_excluded(&filter_excl, &unit_owned, e.path(), is_dir)
    });
    let mut out = std::collections::HashSet::new();
    for e in wb.build().flatten() {
        // Check the excludes again on every entry: `filter_entry` alone let
        // excluded files at `max_depth` through, and this listing decides
        // which changed files are searched before the next rebuild (secrets
        // must never be among them).
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if e.depth() == 1 && !is_excluded(&excl, unit, e.path(), is_dir) {
            out.insert(e.file_name().to_string_lossy().into_owned());
        }
    }
    out
}

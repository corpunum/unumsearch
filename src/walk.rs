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
    let excl = exclude_matcher(cfg, root);
    let mut wb = WalkBuilder::new(root);
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
    wb.filter_entry(move |e| {
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        e.path() == dir_owned || !is_excluded(&excl, &unit_owned, e.path(), is_dir)
    });
    let mut out = std::collections::HashSet::new();
    for e in wb.build().flatten() {
        if e.depth() == 1 {
            out.insert(e.file_name().to_string_lossy().into_owned());
        }
    }
    out
}

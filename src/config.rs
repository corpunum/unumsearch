// SPDX-License-Identifier: Apache-2.0
//! Configuration: defaults, then a TOML file, then environment, then flags.
//! Nothing here knows about any particular machine, agent or framework.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Patterns that are NEVER indexed, whatever the configuration says. A search
/// index is a copy of what it covers; credentials must not end up in one.
pub const SECRET_EXCLUDES: &[&str] = &[
    ".ssh/",
    ".gnupg/",
    ".aws/",
    ".azure/",
    ".kube/",
    ".docker/",
    ".config/",
    ".password-store/",
    ".env",
    ".env.*",
    "*.env",
    ".envrc",
    "*.key",
    "*.pem",
    "*.p12",
    "*.pfx",
    "*.jks",
    "*.keystore",
    "*.kdbx",
    "*.gpg",
    "*.asc",
    "id_rsa*",
    "id_dsa*",
    "id_ecdsa*",
    "id_ed25519*",
    "secrets.json",
    "secrets.*.json",
    "secret.json",
    "*.secret",
    "*.secrets",
    "credentials",
    "credentials.json",
    "*credentials*.json",
    "auth.json",
    "token.json",
    ".netrc",
    ".npmrc",
    ".pypirc",
    ".git-credentials",
    ".htpasswd",
];

/// Build output, dependency trees and binary/media formats. Overridable with
/// `default_excludes = false`.
pub const DEFAULT_EXCLUDES: &[&str] = &[
    ".git/",
    ".hg/",
    ".svn/",
    "node_modules/",
    "target/",
    "dist/",
    "build/",
    ".next/",
    "coverage/",
    ".cache/",
    "__pycache__/",
    ".pytest_cache/",
    ".mypy_cache/",
    ".tox/",
    ".venv/",
    "venv/",
    "site-packages/",
    ".gradle/",
    ".idea/",
    "*.gguf",
    "*.safetensors",
    "*.bin",
    "*.pt",
    "*.pth",
    "*.onnx",
    "*.ckpt",
    "*.h5",
    "*.db",
    "*.db-*",
    "*.sqlite",
    "*.sqlite3",
    "*.sqlite-*",
    "*.parquet",
    "*.arrow",
    "*.npy",
    "*.npz",
    "*.png",
    "*.jpg",
    "*.jpeg",
    "*.gif",
    "*.webp",
    "*.bmp",
    "*.ico",
    "*.tif",
    "*.tiff",
    "*.psd",
    "*.mp4",
    "*.mov",
    "*.webm",
    "*.mkv",
    "*.avi",
    "*.wav",
    "*.mp3",
    "*.flac",
    "*.ogg",
    "*.m4a",
    "*.zip",
    "*.tar",
    "*.gz",
    "*.tgz",
    "*.xz",
    "*.zst",
    "*.7z",
    "*.bz2",
    "*.rar",
    "*.whl",
    "*.jar",
    "*.so",
    "*.so.*",
    "*.dylib",
    "*.dll",
    "*.exe",
    "*.o",
    "*.a",
    "*.lib",
    "*.rlib",
    "*.rmeta",
    "*.class",
    "*.pyc",
    "*.wasm",
    "*.pdf",
    "*.img",
    "*.iso",
    "*.apk",
    "*.dmg",
    "*.dtb",
    "*.ttf",
    "*.otf",
    "*.woff",
    "*.woff2",
    "*.min.js.map",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Directories to index. Each is one unit, unless listed in `split_roots`.
    pub roots: Vec<String>,
    /// Roots whose immediate child directories are indexed as separate units
    /// (e.g. a folder of many checkouts): finer-grained rebuilds.
    pub split_roots: Vec<String>,
    /// Where the index lives. Default: the platform cache dir + /unumsearch.
    pub index_dir: Option<String>,
    /// Extra gitignore-syntax exclude patterns.
    pub excludes: Vec<String>,
    /// Apply DEFAULT_EXCLUDES (secrets are always excluded regardless).
    pub default_excludes: bool,
    /// Respect .gitignore / .ignore / git exclude files (ripgrep semantics).
    pub gitignore: bool,
    /// Include hidden files and directories.
    pub hidden: bool,
    /// Files larger than this are not indexed (bytes).
    pub max_file_size: u64,
    /// Memory budget for index building, in MiB.
    pub max_memory_mb: u64,
    /// HTTP listen address for `serve`.
    pub listen: String,
    /// Quiet period after a change before a unit is rebuilt.
    pub debounce_ms: u64,
    /// Upper bound on how long a continuously-changing unit waits.
    pub max_wait_ms: u64,
    /// Full rescan interval (catches anything the watcher missed).
    pub rescan_secs: u64,
    /// Use filesystem notifications (otherwise only periodic rescans).
    pub watch: bool,
    /// Threads used to verify candidates and walk trees (0 = auto, capped at 8).
    pub threads: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            roots: vec![],
            split_roots: vec![],
            index_dir: None,
            excludes: vec![],
            default_excludes: true,
            gitignore: true,
            hidden: true,
            max_file_size: 1 << 20,
            max_memory_mb: 96,
            listen: "127.0.0.1:7781".into(),
            debounce_ms: 500,
            max_wait_ms: 10_000,
            rescan_secs: 300,
            watch: true,
            threads: 0,
        }
    }
}

pub fn expand_tilde(p: &str) -> PathBuf {
    if p == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(p));
    }
    if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        if let Some(h) = dirs::home_dir() {
            return h.join(rest);
        }
    }
    PathBuf::from(p)
}

/// Lexical normalisation to an absolute path (no symlink resolution: a path
/// the caller passes must map to the same unit the index stored).
pub fn normalize(p: &Path) -> PathBuf {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(p)
    };
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

impl Config {
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("unumsearch").join("config.toml"))
    }

    /// Load from `path` (or $UNUMSEARCH_CONFIG, or the platform default if it
    /// exists), then apply environment overrides.
    pub fn load(path: Option<&Path>) -> Result<Config, String> {
        Self::load_with(path, true)
    }

    /// Like [`Config::load`]; with `use_default` false the platform default
    /// config file is never read (an explicit `path` or $UNUMSEARCH_CONFIG
    /// still is).
    pub fn load_with(path: Option<&Path>, use_default: bool) -> Result<Config, String> {
        let env_path = std::env::var_os("UNUMSEARCH_CONFIG").map(PathBuf::from);
        let chosen = path.map(Path::to_path_buf).or(env_path).or_else(|| {
            if use_default {
                Self::default_path().filter(|p| p.exists())
            } else {
                None
            }
        });
        let mut cfg = match chosen {
            Some(p) => {
                let text = std::fs::read_to_string(&p)
                    .map_err(|e| format!("config {}: {e}", p.display()))?;
                toml::from_str::<Config>(&text)
                    .map_err(|e| format!("config {}: {e}", p.display()))?
            }
            None => Config::default(),
        };
        cfg.apply_env();
        Ok(cfg)
    }

    fn apply_env(&mut self) {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        if let Some(v) = std::env::var_os("UNUMSEARCH_ROOTS") {
            self.roots = std::env::split_paths(&v)
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
        }
        if let Some(v) = std::env::var_os("UNUMSEARCH_SPLIT_ROOTS") {
            self.split_roots = std::env::split_paths(&v)
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
        }
        if let Some(v) = var("UNUMSEARCH_INDEX_DIR") {
            self.index_dir = Some(v);
        }
        if let Some(v) = var("UNUMSEARCH_LISTEN") {
            self.listen = v;
        }
        if let Some(v) = var("UNUMSEARCH_MAX_MEMORY_MB").and_then(|v| v.parse().ok()) {
            self.max_memory_mb = v;
        }
        if let Some(v) = var("UNUMSEARCH_MAX_FILE_SIZE").and_then(|v| v.parse().ok()) {
            self.max_file_size = v;
        }
    }

    pub fn index_dir(&self) -> PathBuf {
        match &self.index_dir {
            Some(d) => normalize(&expand_tilde(d)),
            None => dirs::cache_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("unumsearch"),
        }
    }

    pub fn root_paths(&self) -> Vec<PathBuf> {
        self.roots
            .iter()
            .map(|r| normalize(&expand_tilde(r)))
            .collect()
    }

    pub fn split_paths(&self) -> Vec<PathBuf> {
        self.split_roots
            .iter()
            .map(|r| normalize(&expand_tilde(r)))
            .collect()
    }

    pub fn thread_count(&self) -> usize {
        if self.threads > 0 {
            return self.threads;
        }
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .min(8)
    }

    /// All exclude lines in gitignore syntax, secrets first.
    pub fn exclude_lines(&self) -> Vec<String> {
        let mut v: Vec<String> = SECRET_EXCLUDES.iter().map(|s| s.to_string()).collect();
        if self.default_excludes {
            v.extend(DEFAULT_EXCLUDES.iter().map(|s| s.to_string()));
        }
        v.extend(self.excludes.iter().cloned());
        v
    }
}

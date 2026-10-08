// SPDX-License-Identifier: Apache-2.0
//! unumsearch: always-fresh indexed file and code search.
//!
//! * [`config`]: configuration (file, environment, flags), default and secret excludes.
//! * [`walk`]: the corpus, defined with ripgrep's walker (gitignore, hidden, size).
//! * [`trigram`]: trigram extraction and regex -> trigram query planning.
//! * [`shard`]: the compact on-disk index format (mmap, delta/varint postings).
//! * [`engine`]: units, building, searching, freshness.
//! * [`watch`]: filesystem notifications, debounce, periodic rescans.
//! * [`api`]: the request surface shared by every front-end.

pub mod api;
pub mod config;
pub mod engine;
pub mod shard;
pub mod trigram;
pub mod walk;
pub mod watch;

pub use config::Config;
pub use engine::{Engine, FilesOpts, SearchOpts};

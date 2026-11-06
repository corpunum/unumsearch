// SPDX-License-Identifier: Apache-2.0
//! Thin transports over `unumsearch::api`.
pub mod http;
pub mod mcp;
pub mod rpc;

/// Method names are client-supplied: count only the known ones (no arbitrary strings).
pub(crate) fn known_method(m: &str) -> &'static str {
    match m {
        "search" => "search",
        "files" => "files",
        "lookup" => "lookup",
        "status" => "status",
        "reindex" => "reindex",
        _ => "unknown",
    }
}

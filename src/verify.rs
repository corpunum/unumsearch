// SPDX-License-Identifier: Apache-2.0
//! Candidate verification helpers: reading a file as text, a per-file literal
//! prefilter, and line-by-line matching driven by one whole-buffer search.
//!
//! Every helper here gives exactly the answer of the v0.1.7 per-line loop
//! (`buf.split(b'\n')`, strip one trailing `\r`, `is_match` per line). Where a
//! cheaper route cannot be proven to agree, the caller keeps that loop.

use memchr::{memchr, memchr_iter, memmem, memrchr};
use regex::bytes::Regex;
use regex_syntax::hir::{Hir, HirKind, Look};
use std::fs::File;
use std::path::Path;

/// Bytes read and checked for NUL before the rest of a candidate is read.
pub const SNIFF: usize = 8 * 1024;

/// Read `p` as a text candidate into `buf`. Returns false when the file is
/// unreadable, larger than `cap`, or contains a NUL byte anywhere (the same
/// rule as v0.1.7). The first `SNIFF` bytes are read and checked on their
/// own, so a binary file is rejected before the rest of it is read; the NUL
/// rule itself still covers the whole file, so the answer is unchanged.
pub fn read_text(p: &Path, cap: u64, buf: &mut Vec<u8>) -> bool {
    use std::io::Read;
    buf.clear();
    let Ok(f) = File::open(p) else { return false };
    let hint = f.metadata().map(|m| m.len()).unwrap_or(0);
    if hint > cap {
        return false;
    }
    buf.reserve(hint as usize + 1);
    let head = (&f).take(SNIFF as u64).read_to_end(buf);
    if head.is_err() || memchr(0, buf).is_some() {
        return false;
    }
    let head_len = buf.len();
    if head_len == SNIFF {
        // A short head means end of file: no second read needed.
        let left = cap.saturating_add(1).saturating_sub(head_len as u64);
        if (&f).take(left).read_to_end(buf).is_err() {
            return false;
        }
    }
    buf.len() as u64 <= cap && memchr(0, &buf[head_len..]).is_none()
}

/// Per-query verification plan, built once per search from the same parse
/// the planner uses.
pub struct Prefilter {
    /// The longest literal every match must contain. A file without it
    /// cannot match, so it is skipped before any regex work.
    lit: Option<memmem::Finder<'static>>,
    /// The pattern has only assertions for which one whole-buffer search
    /// agrees with the per-line answer (see `line_fast`).
    fast: bool,
    /// The pattern has `$` (end of line). Per-line `$` sees the `\r` of a
    /// CRLF line differently from the whole buffer, so such files take the
    /// per-line path when they contain a `\r`.
    eol: bool,
}

impl Prefilter {
    pub fn new(pattern: &str, case_insensitive: bool) -> Result<Self, String> {
        let hir = regex_syntax::ParserBuilder::new()
            .case_insensitive(case_insensitive)
            .multi_line(true)
            .build()
            .parse(pattern)
            .map_err(|e| e.to_string())?;
        let mut best = Vec::new();
        required_literal(&hir, &mut best);
        let mut eol = false;
        let fast = assertions_ok(&hir, &mut eol);
        Ok(Self {
            lit: (!best.is_empty()).then(|| memmem::Finder::new(&best).into_owned()),
            fast,
            eol,
        })
    }

    /// False when no match can be in `buf`: the required literal is absent.
    pub fn may_match(&self, buf: &[u8]) -> bool {
        self.lit.as_ref().is_none_or(|f| f.find(buf).is_some())
    }

    /// Whether `buf` may use `line_fast` (otherwise the caller keeps the
    /// per-line loop).
    pub fn line_fast(&self, buf: &[u8]) -> bool {
        self.fast && !(self.eol && memchr(b'\r', buf).is_some())
    }
}

/// The longest literal that every match must contain: literals reached
/// through concatenation, captures and repetitions with a minimum of one
/// (never through alternations or optional parts).
fn required_literal(h: &Hir, best: &mut Vec<u8>) {
    match h.kind() {
        HirKind::Literal(l) => {
            if l.0.len() > best.len() {
                *best = l.0.to_vec();
            }
        }
        HirKind::Capture(c) => required_literal(&c.sub, best),
        HirKind::Repetition(r) if r.min >= 1 => required_literal(&r.sub, best),
        HirKind::Concat(subs) => subs.iter().for_each(|s| required_literal(s, best)),
        _ => {}
    }
}

/// True when a single whole-buffer search can stand in for the per-line
/// test. Required: no assertion that looks past a line boundary (`\A`, `\z`,
/// `\Z`, CRLF-aware anchors, the half-word boundaries). `$` is allowed but
/// recorded in `eol`; word boundaries and `^` see the same context per line
/// and in the whole buffer, because `\n` is neither a word byte nor
/// something `^` cares about beyond the line start.
fn assertions_ok(h: &Hir, eol: &mut bool) -> bool {
    match h.kind() {
        HirKind::Look(l) => match l {
            Look::StartLF
            | Look::WordAscii
            | Look::WordAsciiNegate
            | Look::WordUnicode
            | Look::WordUnicodeNegate => true,
            Look::EndLF => {
                *eol = true;
                true
            }
            _ => false,
        },
        HirKind::Capture(c) => assertions_ok(&c.sub, eol),
        HirKind::Repetition(r) => assertions_ok(&r.sub, eol),
        HirKind::Concat(s) | HirKind::Alternation(s) => s.iter().all(|x| assertions_ok(x, eol)),
        _ => true,
    }
}

/// Call `emit(line, text)` for every line of `buf` that matches on its own
/// (0-based line index; `text` is the line without its `\n` and one trailing
/// `\r`), in ascending order, until `emit` returns false. Gives the same
/// lines as testing every `buf.split(b'\n')` piece with `re.is_match`, given
/// `Prefilter::line_fast(buf)`.
///
/// Each step searches the buffer from the start of the current line. The
/// leftmost match found there either starts on that line (the line is then
/// tested on its own) or starts later, which proves no line before it can
/// match: a per-line match at `a` is also a buffer match starting at `a`. A
/// match that spans a `\n` therefore cannot hide a later line, because the
/// search never resumes from inside a previous match.
pub fn matching_lines<'a>(
    re: &Regex,
    buf: &'a [u8],
    mut emit: impl FnMut(usize, &'a [u8]) -> bool,
) {
    let len = buf.len();
    let mut pos = 0usize; // start of the line `line` (always a line start)
    let mut line = 0usize; // 0-based index of the line starting at `pos`
    while let Some(m) = re.find_at(buf, pos) {
        let s = m.start();
        // Move to the line that holds `s`: count the newlines before it.
        let before = &buf[pos..s];
        line += memchr_iter(b'\n', before).count();
        let ls = memrchr(b'\n', before).map_or(pos, |i| pos + i + 1);
        let le = memchr(b'\n', &buf[ls..]).map_or(len, |i| ls + i);
        let mut text = &buf[ls..le];
        if let Some(t) = text.strip_suffix(b"\r") {
            text = t;
        }
        if re.is_match(text) && !emit(line, text) {
            return;
        }
        if le >= len {
            return;
        }
        pos = le + 1;
        line += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The v0.1.7 per-line rule, spelled out.
    fn naive(re: &Regex, buf: &[u8]) -> Vec<(usize, Vec<u8>)> {
        let mut out = Vec::new();
        for (ln, line) in buf.split(|b| *b == b'\n').enumerate() {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if re.is_match(line) {
                out.push((ln, line.to_vec()));
            }
        }
        out
    }

    fn fast(re: &Regex, buf: &[u8]) -> Vec<(usize, Vec<u8>)> {
        let mut out = Vec::new();
        matching_lines(re, buf, |ln, t| {
            out.push((ln, t.to_vec()));
            true
        });
        out
    }

    fn re(p: &str) -> Regex {
        regex::bytes::RegexBuilder::new(p)
            .multi_line(true)
            .build()
            .unwrap()
    }

    /// Patterns that take the fast path (no `$`, `\A`, `\z`), each checked
    /// against the naive rule on the same buffers.
    const FAST: &[&str] = &[
        "fn\\s+main",
        "main",
        "^$",
        "^",
        "a*",
        "x*",
        "\\bfoo\\b",
        "foo\\W",
        "f.o",
        "[^a]+",
        "\\s",
        "(?s)a.b",
        "a\\nb",
        "\\r",
        "^\\s*$",
        "é",
        "(?i)FOO",
    ];

    /// Compare the fast path with the naive rule, but only where the engine
    /// would take the fast path (`line_fast`); elsewhere it keeps the naive
    /// loop, so there is nothing to compare.
    fn check(p: &str, buf: &[u8]) {
        let r = re(p);
        if !Prefilter::new(p, false).unwrap().line_fast(buf) {
            assert!(
                p.contains('$'),
                "only `$` with CR may leave the fast path: {p:?}"
            );
            return;
        }
        assert_eq!(
            fast(&r, buf),
            naive(&r, buf),
            "pattern {p:?} buffer {:?}",
            String::from_utf8_lossy(buf)
        );
    }

    #[test]
    fn crlf_lines_match_on_stripped_text() {
        // `foo$` only matches a CRLF line once the `\r` is stripped; the
        // fast path must not be used for such a file (`line_fast` is false).
        let buf = b"foo\r\nbar\r\n";
        let p = Prefilter::new("foo$", false).unwrap();
        assert!(!p.line_fast(buf));
        let r = re("foo$");
        assert_eq!(naive(&r, buf), vec![(0, b"foo".to_vec())]);
        for b in FAST {
            check(b, buf);
        }
        // Without the CR the same `$` pattern may use the fast path.
        assert!(Prefilter::new("foo$", false)
            .unwrap()
            .line_fast(b"foo\nbar\n"));
    }

    #[test]
    fn no_trailing_newline() {
        let buf = b"alpha\nbeta fn main";
        check("main", buf);
        assert_eq!(fast(&re("main"), buf), vec![(1, b"beta fn main".to_vec())]);
    }

    #[test]
    fn match_at_eof_and_empty_last_line() {
        // A trailing newline makes an empty last line that `^` matches.
        check("^", b"a\n");
        check("^$", b"a\n");
        assert_eq!(fast(&re("^$"), b"a\n"), vec![(1, Vec::new())]);
        assert_eq!(
            fast(&re("^$"), b"a\n\n"),
            vec![(1, Vec::new()), (2, Vec::new())]
        );
        // Empty match at the very end of the buffer.
        check("x*", b"ab");
        check("x*", b"");
        check("main", b"main");
    }

    #[test]
    fn empty_lines_and_blank_only_buffers() {
        for buf in [&b""[..], b"\n", b"\n\n\n", b"\r\n\r\n", b"a\n\nb\n\n"] {
            for b in FAST {
                check(b, buf);
            }
        }
    }

    #[test]
    fn long_lines_keep_numbers() {
        let mut buf = vec![b'x'; 5000];
        buf.push(b'\n');
        buf.extend_from_slice(b"fn main\n");
        buf.extend(std::iter::repeat_n(b'y', 3000));
        buf.extend_from_slice(b"\nfn\tmain\n");
        check("fn\\s+main", &buf);
        assert_eq!(
            fast(&re("fn\\s+main"), &buf)
                .iter()
                .map(|(l, _)| *l)
                .collect::<Vec<_>>(),
            vec![1, 3]
        );
    }

    #[test]
    fn match_spanning_newline_does_not_hide_later_lines() {
        // `fn\s+main` matches "fn\n main" across lines 0 and 1; line 2 is a
        // real per-line match that must still be reported.
        let buf = b"fn\n main\nfn main\n";
        check("fn\\s+main", buf);
        assert_eq!(fast(&re("fn\\s+main"), buf), vec![(2, b"fn main".to_vec())]);
        // Overlapping spans: a cross-line match starting inside a per-line one.
        check("a\\s*a|aa", b"a\na\naa\n");
        check("\\s+x", b"x\n x\n\n x");
    }

    #[test]
    fn randomised_agreement_with_naive_rule() {
        let alphabet = [
            b'a', b'f', b'o', b'\n', b'\r', b' ', b'\t', 0xC3, 0xA9, b'x',
        ];
        let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..300 {
            let n = (next() % 60) as usize;
            let buf: Vec<u8> = (0..n)
                .map(|_| alphabet[(next() % alphabet.len() as u64) as usize])
                .collect();
            for p in FAST {
                check(p, &buf);
            }
        }
    }

    #[test]
    fn required_literal_is_the_longest_mandatory_run() {
        let lit = |p: &str| {
            let hir = regex_syntax::Parser::new().parse(p).unwrap();
            let mut best = Vec::new();
            required_literal(&hir, &mut best);
            String::from_utf8(best).unwrap()
        };
        assert_eq!(lit("fn\\s+main"), "main");
        assert_eq!(lit("(ab)+cdef"), "cdef");
        assert_eq!(lit("(ab)+cd"), "ab");
        assert_eq!(lit("ab|cdef"), "");
        assert_eq!(lit("x(hello)?"), "x");
    }

    #[test]
    fn prefilter_never_rejects_a_match() {
        let pats = ["fn\\s+main", "(?i)FN MAIN", "needle", "a|bcd", "\\bmain\\b"];
        let bufs: [&[u8]; 4] = [b"fn\n main\n", b"FN MAIN", b"needle in a haystack", b"xyz"];
        for p in pats {
            let pf = Prefilter::new(p, p.contains("(?i)")).unwrap();
            let r = regex::bytes::RegexBuilder::new(p)
                .case_insensitive(p.contains("(?i)"))
                .multi_line(true)
                .build()
                .unwrap();
            for b in bufs {
                if r.is_match(b) {
                    assert!(pf.may_match(b), "prefilter rejected a match of {p:?}");
                }
            }
        }
    }

    #[test]
    fn read_text_rules() {
        let dir = std::env::temp_dir().join(format!("unumsearch-readtext-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let text = dir.join("text.txt");
        std::fs::write(&text, b"hello\nworld\n").unwrap();
        let mut buf = Vec::new();
        assert!(read_text(&text, 1 << 20, &mut buf));
        assert_eq!(buf, b"hello\nworld\n");
        assert!(!read_text(&text, 3, &mut buf), "over the size cap");

        // A NUL after the sniff window still makes the file binary.
        let late = dir.join("late.bin");
        let mut data = vec![b'a'; SNIFF * 3];
        data.push(0);
        std::fs::write(&late, &data).unwrap();
        assert!(!read_text(&late, 1 << 30, &mut buf));

        // Exactly at the cap is allowed.
        let exact = dir.join("exact.txt");
        std::fs::write(&exact, vec![b'a'; SNIFF + 5]).unwrap();
        assert!(read_text(&exact, (SNIFF + 5) as u64, &mut buf));
        assert!(!read_text(&exact, (SNIFF + 4) as u64, &mut buf));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

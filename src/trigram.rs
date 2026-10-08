// SPDX-License-Identifier: Apache-2.0
//! Trigrams: extraction from content, and a query plan derived from a regex.
//!
//! The index stores, per document, the set of byte trigrams of its content
//! with ASCII letters folded to lowercase. A query is turned into a boolean
//! formula over trigrams that every matching document MUST satisfy; the plan
//! may be weaker than the regex (more candidates) but never stronger, because
//! every candidate is verified against the real file afterwards.

use regex_syntax::hir::{Class, Hir, HirKind};
use std::collections::BTreeSet;

pub type Tri = u32;

#[inline]
pub fn fold(b: u8) -> u8 {
    b.to_ascii_lowercase()
}

#[inline]
pub fn pack(a: u8, b: u8, c: u8) -> Tri {
    ((a as u32) << 16) | ((b as u32) << 8) | c as u32
}

/// One bit of an 8-bit "next byte" mask.
#[inline]
pub fn next_bit(b: u8) -> u8 {
    1 << ((b ^ (b >> 3)) & 7)
}

/// Distinct folded trigrams of `content` with, for each, a mask of the
/// (folded) bytes that follow it anywhere in the document. The mask lets a
/// query require "abc followed by d" -- adjacency the bare trigram set loses --
/// at the cost of one byte per posting. Sorted by trigram, into `out`.
pub fn extract(content: &[u8], out: &mut Vec<(Tri, u8)>) {
    out.clear();
    if content.len() < 3 {
        return;
    }
    let f: Vec<u8> = content.iter().map(|b| fold(*b)).collect();
    for i in 0..f.len() - 2 {
        let m = if i + 3 < f.len() {
            next_bit(f[i + 3])
        } else {
            0
        };
        out.push((pack(f[i], f[i + 1], f[i + 2]), m));
    }
    out.sort_unstable_by_key(|x| x.0);
    let mut w = 0usize;
    for r in 0..out.len() {
        if w > 0 && out[w - 1].0 == out[r].0 {
            out[w - 1].1 |= out[r].1;
        } else {
            out[w] = out[r];
            w += 1;
        }
    }
    out.truncate(w);
}

/// A boolean formula over trigrams.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    /// No constraint: every document is a candidate.
    All,
    Tri(Tri),
    /// The trigram, followed somewhere by this (folded) byte.
    TriNext(Tri, u8),
    And(Vec<Plan>),
    Or(Vec<Plan>),
}

impl Plan {
    fn and(parts: Vec<Plan>) -> Plan {
        let mut v: Vec<Plan> = Vec::new();
        for p in parts {
            match p {
                Plan::All => {}
                Plan::And(xs) => v.extend(xs),
                x => v.push(x),
            }
        }
        v.dedup();
        match v.len() {
            0 => Plan::All,
            1 => v.pop().unwrap(),
            _ => Plan::And(v),
        }
    }
    fn or(parts: Vec<Plan>) -> Plan {
        let mut v: Vec<Plan> = Vec::new();
        for p in parts {
            match p {
                Plan::All => return Plan::All,
                Plan::Or(xs) => v.extend(xs),
                x => v.push(x),
            }
        }
        v.dedup();
        match v.len() {
            0 => Plan::All,
            1 => v.pop().unwrap(),
            _ => Plan::Or(v),
        }
    }
}

const MAX_SET: usize = 64;
const MAX_CLASS: usize = 16;

/// Per-node analysis: the exact (folded) set of strings the node can match,
/// when that set is small, otherwise just a necessary plan.
struct Info {
    exact: Option<BTreeSet<Vec<u8>>>,
    plan: Plan,
}

impl Info {
    fn exact(set: BTreeSet<Vec<u8>>) -> Info {
        Info {
            exact: Some(set),
            plan: Plan::All,
        }
    }
    fn plan(p: Plan) -> Info {
        Info {
            exact: None,
            plan: p,
        }
    }
}

struct Planner {
    case_insensitive: bool,
}

impl Planner {
    fn string_plan(&self, s: &[u8]) -> Plan {
        if s.len() < 3 {
            return Plan::All;
        }
        let mut parts = Vec::new();
        for i in 0..s.len() - 2 {
            let w = &s[i..i + 3];
            // ASCII is folded in the index; non-ASCII bytes are stored raw, so
            // under case-insensitive matching a trigram touching them is not a
            // reliable requirement (its other-case form has different bytes).
            if self.case_insensitive && w.iter().any(|b| *b >= 0x80) {
                continue;
            }
            let t = pack(w[0], w[1], w[2]);
            match s.get(i + 3) {
                Some(&n) if !(self.case_insensitive && n >= 0x80) => {
                    parts.push(Plan::TriNext(t, n))
                }
                _ => parts.push(Plan::Tri(t)),
            }
        }
        Plan::and(parts)
    }

    fn to_plan(&self, i: Info) -> Plan {
        match i.exact {
            Some(set) => Plan::or(set.iter().map(|s| self.string_plan(s)).collect()),
            None => i.plan,
        }
    }

    fn folded(bytes: &[u8]) -> Vec<u8> {
        bytes.iter().map(|b| fold(*b)).collect()
    }

    fn concat(&self, a: Info, b: Info) -> Info {
        if let (Some(x), Some(y)) = (&a.exact, &b.exact) {
            if x.len() * y.len() <= MAX_SET {
                let mut out = BTreeSet::new();
                for p in x {
                    for q in y {
                        let mut s = p.clone();
                        s.extend_from_slice(q);
                        out.insert(s);
                    }
                }
                return Info::exact(out);
            }
        }
        Info::plan(Plan::and(vec![self.to_plan(a), self.to_plan(b)]))
    }

    fn analyze(&self, h: &Hir) -> Info {
        match h.kind() {
            HirKind::Empty | HirKind::Look(_) => Info::exact(BTreeSet::from([vec![]])),
            HirKind::Literal(lit) => Info::exact(BTreeSet::from([Self::folded(&lit.0)])),
            HirKind::Class(c) => match class_strings(c) {
                Some(set) => Info::exact(set.into_iter().map(|s| Self::folded(&s)).collect()),
                None => Info::plan(Plan::All),
            },
            HirKind::Capture(c) => self.analyze(&c.sub),
            HirKind::Repetition(r) => {
                if r.min == 0 {
                    if r.max == Some(1) {
                        let sub = self.analyze(&r.sub);
                        if let Some(mut set) = sub.exact {
                            if set.len() < MAX_SET {
                                set.insert(vec![]);
                                return Info::exact(set);
                            }
                        }
                    }
                    Info::plan(Plan::All)
                } else if r.min == 1 && r.max == Some(1) {
                    self.analyze(&r.sub)
                } else {
                    // At least one occurrence is required.
                    let sub = self.analyze(&r.sub);
                    Info::plan(self.to_plan(sub))
                }
            }
            HirKind::Concat(subs) => {
                let mut acc = Info::exact(BTreeSet::from([vec![]]));
                for s in subs {
                    let next = self.analyze(s);
                    acc = self.concat(acc, next);
                }
                acc
            }
            HirKind::Alternation(subs) => {
                let infos: Vec<Info> = subs.iter().map(|s| self.analyze(s)).collect();
                if infos.iter().all(|i| i.exact.is_some()) {
                    let total: usize = infos.iter().map(|i| i.exact.as_ref().unwrap().len()).sum();
                    if total <= MAX_SET {
                        let mut out = BTreeSet::new();
                        for i in infos {
                            out.extend(i.exact.unwrap());
                        }
                        return Info::exact(out);
                    }
                }
                Info::plan(Plan::or(
                    infos.into_iter().map(|i| self.to_plan(i)).collect(),
                ))
            }
        }
    }
}

/// Enumerate a small character class as UTF-8 strings.
fn class_strings(c: &Class) -> Option<BTreeSet<Vec<u8>>> {
    let mut out = BTreeSet::new();
    match c {
        Class::Unicode(u) => {
            let mut n = 0usize;
            for r in u.ranges() {
                n += (r.end() as usize).saturating_sub(r.start() as usize) + 1;
                if n > MAX_CLASS {
                    return None;
                }
            }
            for r in u.ranges() {
                for cp in (r.start() as u32)..=(r.end() as u32) {
                    if let Some(ch) = char::from_u32(cp) {
                        let mut buf = [0u8; 4];
                        out.insert(ch.encode_utf8(&mut buf).as_bytes().to_vec());
                    }
                }
            }
        }
        Class::Bytes(b) => {
            let mut n = 0usize;
            for r in b.ranges() {
                n += (r.end() as usize) - (r.start() as usize) + 1;
                if n > MAX_CLASS {
                    return None;
                }
            }
            for r in b.ranges() {
                for x in r.start()..=r.end() {
                    out.insert(vec![x]);
                }
            }
        }
    }
    Some(out)
}

/// Escape a literal so it can be planned and matched as a regex.
pub fn escape(lit: &str) -> String {
    regex_syntax::escape(lit)
}

/// Build the trigram plan for `pattern` (regex syntax). Errors mean the
/// pattern is not a valid regex.
pub fn plan(pattern: &str, case_insensitive: bool) -> Result<Plan, String> {
    let hir = regex_syntax::ParserBuilder::new()
        .case_insensitive(case_insensitive)
        .multi_line(true)
        .build()
        .parse(pattern)
        .map_err(|e| e.to_string())?;
    let p = Planner { case_insensitive };
    let info = p.analyze(&hir);
    Ok(p.to_plan(info))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Plan {
        let b = s.as_bytes();
        if b.len() == 4 {
            return Plan::TriNext(pack(b[0], b[1], b[2]), b[3]);
        }
        Plan::Tri(pack(b[0], b[1], b[2]))
    }

    #[test]
    fn literal_needs_all_trigrams() {
        assert_eq!(
            plan("abcd", false).unwrap(),
            Plan::And(vec![t("abcd"), t("bcd")])
        );
    }

    #[test]
    fn case_insensitive_folds() {
        assert_eq!(
            plan("ABcD", true).unwrap(),
            Plan::And(vec![t("abcd"), t("bcd")])
        );
    }

    #[test]
    fn alternation_is_or() {
        assert_eq!(
            plan("abc|xyz", false).unwrap(),
            Plan::Or(vec![t("abc"), t("xyz")])
        );
    }

    #[test]
    fn short_or_optional_is_unconstrained() {
        assert_eq!(plan("ab", false).unwrap(), Plan::All);
        assert_eq!(plan("a.*b", false).unwrap(), Plan::All);
        assert_eq!(plan("(abc)?", false).unwrap(), Plan::All);
    }

    #[test]
    fn concat_across_wildcard_keeps_both_sides() {
        let p = plan("foo.*bar", false).unwrap();
        assert_eq!(p, Plan::And(vec![t("foo"), t("bar")]));
    }

    #[test]
    fn small_class_expands() {
        let p = plan("ab[cd]", false).unwrap();
        assert_eq!(p, Plan::Or(vec![t("abc"), t("abd")]));
    }

    #[test]
    fn invalid_regex_errors() {
        assert!(plan("(", false).is_err());
    }
}

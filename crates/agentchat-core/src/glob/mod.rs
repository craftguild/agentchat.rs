//! Exact bounded intersection of anchored glob languages and normalized paths.

mod syntax;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::OnceLock;

use regex_automata::{
    Anchored, Input,
    dfa::{Automaton, StartKind, dense},
    util::primitives::StateID,
};

use crate::{Error, ErrorCode, Result};

type Dfa = dense::DFA<Vec<u32>>;
const MAX_AUTOMATON_BYTES: usize = 4 * 1_024 * 1_024;
/// Maximum reachable states in an overlap product, including the path constraint.
pub const MAX_PRODUCT_STATES: usize = 100_000;

/// An anchored pattern whose matching and overlap semantics are identical.
#[derive(Debug)]
pub struct Glob {
    pattern: String,
    dfa: Dfa,
}

impl Glob {
    /// Compile a v1 glob without inspecting any existing files.
    pub fn compile(pattern: &str) -> Result<Self> {
        let regex = syntax::regex(pattern)?;
        // Parse separately so malformed ranges are input errors, while a DFA
        // determinization limit is always an explicit complexity failure.
        regex_syntax::Parser::new()
            .parse(&regex)
            .map_err(|_| syntax::invalid("Invalid character class or pattern syntax."))?;
        let dfa = build(&regex)?;
        Ok(Self {
            pattern: pattern.to_owned(),
            dfa,
        })
    }

    /// The validated source pattern (trailing slash retains its subtree meaning).
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    /// Match a case-sensitive, normalized, valid UTF-8 logical relative path.
    pub fn is_match(&self, path: &str) -> bool {
        accepts(&self.dfa, path.as_bytes()) && accepts(normalized(), path.as_bytes())
    }

    /// Decide exact language overlap and return a shortest witness when present.
    pub fn overlap(&self, other: &Self) -> Result<Option<String>> {
        self.overlap_with_limit(other, MAX_PRODUCT_STATES)
    }

    /// Apply a stricter product-state budget for diagnostics or bounded callers.
    ///
    /// Limits above the v1 maximum are clamped. Exhaustion never means disjoint.
    pub fn overlap_with_limit(&self, other: &Self, limit: usize) -> Result<Option<String>> {
        if limit == 0 {
            return Err(syntax::complex());
        }
        let limit = limit.min(MAX_PRODUCT_STATES);
        let a = &self.dfa;
        let b = &other.dfa;
        let n = normalized();
        let initial = (start(a), start(b), start(n));
        let mut seen = HashMap::from([(initial, 0)]);
        let mut nodes = vec![(initial, None::<(usize, u8)>)];
        let mut queue = VecDeque::from([0]);
        // A byte-equivalence representative is sufficient only when all three
        // automata agree on its class. This also bounds branching for Unicode.
        let mut classes = HashSet::new();
        let alphabet: Vec<u8> = [b'a', b'b', b'x', b'0', b'.', b'/']
            .into_iter()
            .chain(0..=255)
            .filter(|&c| {
                classes.insert((
                    a.byte_classes().get(c),
                    b.byte_classes().get(c),
                    n.byte_classes().get(c),
                ))
            })
            .collect();
        while let Some(index) = queue.pop_front() {
            let ((sa, sb, sn), _) = nodes[index];
            // DFA matches are delayed; EOI must be consumed in every component.
            // Testing current match flags would lose anchored end-of-path matches.
            if [(a, sa), (b, sb), (n, sn)]
                .iter()
                .all(|(dfa, state)| dfa.is_match_state(dfa.next_eoi_state(*state)))
            {
                let mut bytes = Vec::new();
                let mut cursor = index;
                while let Some((parent, byte)) = nodes[cursor].1 {
                    bytes.push(byte);
                    cursor = parent;
                }
                bytes.reverse();
                let witness = String::from_utf8(bytes).map_err(|_| {
                    Error::new(
                        ErrorCode::IntegrityError,
                        "Normalized-path automaton accepted invalid UTF-8.",
                    )
                })?;
                return Ok(Some(witness));
            }
            for &byte in &alphabet {
                let next = (
                    a.next_state(sa, byte),
                    b.next_state(sb, byte),
                    n.next_state(sn, byte),
                );
                if a.is_dead_state(next.0)
                    || b.is_dead_state(next.1)
                    || n.is_dead_state(next.2)
                    || seen.contains_key(&next)
                {
                    continue;
                }
                if nodes.len() == limit {
                    return Err(syntax::complex());
                }
                let next_index = nodes.len();
                seen.insert(next, next_index);
                nodes.push((next, Some((index, byte))));
                queue.push_back(next_index);
            }
        }
        Ok(None)
    }
}

fn build(regex: &str) -> Result<Dfa> {
    let dfa = dense::Builder::new()
        .configure(
            dense::Config::new()
                .start_kind(StartKind::Anchored)
                .dfa_size_limit(Some(MAX_AUTOMATON_BYTES))
                .determinize_size_limit(Some(MAX_AUTOMATON_BYTES)),
        )
        .build(regex)
        .map_err(|_| syntax::complex())?;
    if dfa.memory_usage() > MAX_AUTOMATON_BYTES {
        return Err(syntax::complex());
    }
    Ok(dfa)
}

fn start(dfa: &Dfa) -> StateID {
    dfa.start_state_forward(&Input::new(b"").anchored(Anchored::Yes))
        .expect("anchored-only DFA accepts an anchored start configuration")
}

fn accepts(dfa: &Dfa, bytes: &[u8]) -> bool {
    let mut state = start(dfa);
    for &byte in bytes {
        state = dfa.next_state(state, byte);
    }
    dfa.is_match_state(dfa.next_eoi_state(state))
}

fn normalized() -> &'static Dfa {
    static NORMALIZED: OnceLock<Dfa> = OnceLock::new();
    NORMALIZED.get_or_init(|| {
        // Every component has a non-dot character or at least three dots.
        // The language excludes empty, '.', '..', absolute paths and NUL, and
        // Unicode regex semantics constrain the byte language to valid UTF-8.
        let component = r"(?:[^/.\x00][^/\x00]*|\.[^/.\x00][^/\x00]*|\.\.[^/\x00]+)";
        build(&format!(r"\A{component}(?:/{component})*\z"))
            .expect("fixed normalized-path automaton fits the compiled budget")
    })
}

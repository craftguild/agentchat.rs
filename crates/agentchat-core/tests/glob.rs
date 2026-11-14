//! T11: exact language overlap, normalized paths and fail-closed budgets.

use agentchat_core::{ErrorCode, glob::Glob};
use proptest::prelude::*;

#[test]
fn supported_syntax_and_future_paths() {
    let cases = [
        ("src/**", "src/new/deep/future.rs", true),
        ("src/", "src/new/deep/future.rs", true),
        ("src/**/lib.rs", "src/lib.rs", true),
        ("src/**/lib.rs", "src/new/lib.rs", true),
        ("**/lib.rs", "lib.rs", true),
        ("a/**/b/**", "a/b", true),
        ("**", "a", true),
        ("**/**", "a/b", true),
        ("src/*.rs", "src/new/a.rs", false),
        ("src/[a-c]?.{rs,toml}", "src/bβ.rs", true),
        ("src/[!x]*", "src/x.rs", false),
        ("src/[!x]*", "src/a.rs", true),
        (r"a/\*.rs", "a/*.rs", true),
        (r"a/\[x\]", "a/[x]", true),
        ("{a,{b,c}}/x", "c/x", true),
        ("{src/**,tests}/x", "src/new/x", true),
        ("?", "🦀", true),
        ("??", "🦀", false),
        ("*", "", false),
        ("**", "a/../b", false),
        ("**", "./a", false),
        ("**", "a//b", false),
        ("**", "/a", false),
        ("**", "a\0b", false),
        ("**", "...", true),
        ("**", ".git/config", true),
        ("[.]", ".", false),
    ];
    for (pattern, path, expected) in cases {
        let glob = Glob::compile(pattern).unwrap();
        assert_eq!(glob.is_match(path), expected, "{pattern:?}, {path:?}");
    }
}

#[test]
fn overlap_witnesses_and_empty_languages() {
    for (a, b, expected) in [
        ("src/**", "src/future.rs", true),
        ("src/*.rs", "src/*.toml", false),
        ("src/**/x", "src/x/**", true),
        ("a?", "[ab]b", true),
        ("a?", "bb", false),
        ("{a,b}", "{c,d}", false),
        ("[.]", "**", false),
        ("[.][.]", "**", false),
        ("[.][.][.]", "**", true),
        ("?", "🦀", true),
        ("[!a]", "[a]", false),
        ("a/**/b", "a/c/**", true),
        ("a/**/b", "a/**/c", false),
    ] {
        let a = Glob::compile(a).unwrap();
        let b = Glob::compile(b).unwrap();
        let witness = a.overlap(&b).unwrap();
        assert_eq!(
            witness.is_some(),
            expected,
            "{} and {}",
            a.pattern(),
            b.pattern()
        );
        assert_eq!(witness.is_some(), b.overlap(&a).unwrap().is_some());
        if let Some(w) = witness {
            assert!(a.is_match(&w) && b.is_match(&w), "{w:?}");
        }
    }
}

#[test]
fn malformed_and_exhausted_patterns_fail_closed() {
    for p in [
        "",
        "/x",
        "a//b",
        "../a",
        "a/./b",
        "a/{b,..}",
        r"a/\.\./b",
        "a**",
        "a/**b",
        "[",
        "[]",
        "[z-a]",
        "{a}",
        "{a,}",
        "{a,b",
        "a}",
        "a\\",
        "a\0b",
        "!(a)",
        "@(a)",
        "+(a)",
        "?(a)",
        "*(a)",
        "!a",
    ] {
        assert_eq!(
            Glob::compile(p).unwrap_err().code,
            ErrorCode::InvalidArgument,
            "{p:?}"
        );
    }
    assert_eq!(
        Glob::compile(&"a".repeat(1_025)).unwrap_err().code,
        ErrorCode::PatternTooComplex
    );
    let branches = "{a,b}".repeat(11);
    assert_eq!(
        Glob::compile(&branches).unwrap_err().code,
        ErrorCode::PatternTooComplex
    );
    let a = Glob::compile("a*").unwrap();
    let b = Glob::compile("*b").unwrap();
    assert_eq!(
        a.overlap_with_limit(&b, 1).unwrap_err().code,
        ErrorCode::PatternTooComplex
    );
}

fn words(maximum: usize) -> Vec<String> {
    let mut words = vec![String::new()];
    let mut frontier = vec![String::new()];
    for _ in 0..maximum {
        frontier = frontier
            .iter()
            .flat_map(|s| ['a', 'b'].map(|c| format!("{s}{c}")))
            .collect();
        words.extend(frontier.clone());
    }
    words
}

// Independent wildcard evaluator for the generated small-language oracle.
// No production regex, DFA or matching helper participates in this decision.
fn oracle(pattern: &str, path: &str) -> bool {
    fn component(p: &[u8], s: &[u8]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some(b'*') => component(&p[1..], s) || (!s.is_empty() && component(p, &s[1..])),
            Some(b'?') => !s.is_empty() && component(&p[1..], &s[1..]),
            Some(b'[') => !s.is_empty() && b"ab".contains(&s[0]) && component(&p[4..], &s[1..]),
            Some(c) => s.first() == Some(c) && component(&p[1..], &s[1..]),
        }
    }
    fn path_match(p: &[&str], s: &[&str]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some(&"**") => path_match(&p[1..], s) || (!s.is_empty() && path_match(p, &s[1..])),
            Some(c) => {
                !s.is_empty()
                    && component(c.as_bytes(), s[0].as_bytes())
                    && path_match(&p[1..], &s[1..])
            }
        }
    }
    !path.is_empty()
        && path_match(
            &pattern.split('/').collect::<Vec<_>>(),
            &path.split('/').collect::<Vec<_>>(),
        )
}

#[test]
fn exhaustive_small_unbounded_language_oracle() {
    let atoms = ["a", "b", "?", "*", "a*", "*b", "[ab]", "**"];
    let patterns: Vec<_> = atoms
        .iter()
        .map(|s| s.to_string())
        .chain(
            atoms
                .iter()
                .flat_map(|a| atoms.iter().map(move |b| format!("{a}/{b}"))),
        )
        .collect();
    let components: Vec<_> = words(2).into_iter().filter(|w| !w.is_empty()).collect();
    let paths: Vec<_> = components
        .iter()
        .cloned()
        .chain(
            components
                .iter()
                .flat_map(|a| components.iter().map(move |b| format!("{a}/{b}"))),
        )
        .collect();
    let compiled: Vec<_> = patterns.iter().map(|p| Glob::compile(p).unwrap()).collect();
    let languages: Vec<Vec<_>> = patterns
        .iter()
        .enumerate()
        .map(|(i, p)| {
            paths
                .iter()
                .map(|path| {
                    let expected = oracle(p, path);
                    assert_eq!(compiled[i].is_match(path), expected, "{p} on {path}");
                    expected
                })
                .collect()
        })
        .collect();
    for i in 0..patterns.len() {
        for j in i..patterns.len() {
            let expected = languages[i]
                .iter()
                .zip(&languages[j])
                .any(|(a, b)| *a && *b);
            let witness = compiled[i].overlap(&compiled[j]).unwrap();
            assert_eq!(
                witness.is_some(),
                expected,
                "{} vs {}",
                patterns[i],
                patterns[j]
            );
            if let Some(w) = witness {
                assert!(oracle(&patterns[i], &w) && oracle(&patterns[j], &w));
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]
    #[test]
    fn generated_finite_languages_have_exact_intersection(
        a in prop::collection::vec(prop::sample::select(vec!["a", "b", "[ab]", "?"]), 1..5),
        b in prop::collection::vec(prop::sample::select(vec!["a", "b", "[ab]", "?"]), 1..5),
    ) {
        let a = a.concat(); let b = b.concat();
        let expected = words(4).iter().any(|s| oracle(&a,s) && oracle(&b,s));
        let ga = Glob::compile(&a).unwrap(); let gb = Glob::compile(&b).unwrap();
        let witness = ga.overlap(&gb).unwrap();
        prop_assert_eq!(witness.is_some(), expected);
        if let Some(w) = witness { prop_assert!(oracle(&a,&w) && oracle(&b,&w)); }
    }
}

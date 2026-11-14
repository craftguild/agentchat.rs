//! Constructive finite-language oracles cover syntax without a second glob parser.

use std::collections::BTreeSet;

use agentchat_core::{ErrorCode, glob::Glob};

#[derive(Clone)]
struct Finite {
    source: String,
    words: BTreeSet<String>,
}

impl Finite {
    fn literal(word: &str) -> Self {
        let source = word
            .chars()
            .flat_map(|c| {
                let escaped = "\\*?[]{}!,".contains(c);
                escaped.then_some('\\').into_iter().chain([c])
            })
            .collect();
        Self {
            source,
            words: BTreeSet::from([word.into()]),
        }
    }

    fn class(source: &str, members: &[char]) -> Self {
        Self {
            source: source.into(),
            words: members.iter().map(char::to_string).collect(),
        }
    }

    fn then(&self, other: &Self) -> Self {
        Self {
            source: format!("{}{}", self.source, other.source),
            words: self
                .words
                .iter()
                .flat_map(|a| other.words.iter().map(move |b| format!("{a}{b}")))
                .collect(),
        }
    }

    fn either(&self, other: &Self) -> Self {
        Self {
            source: format!("{{{},{}}}", self.source, other.source),
            words: self.words.union(&other.words).cloned().collect(),
        }
    }
}

fn normalized(path: &str) -> bool {
    !path.contains('\0') && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

fn examples() -> Vec<Finite> {
    let mut atoms: Vec<_> = [
        "a", "b", "*", "?", "[", "]", "{x,y}", "\\", "!x", "é", "e\u{301}", "🦀", "\n",
    ]
    .into_iter()
    .map(Finite::literal)
    .collect();
    atoms.extend([
        Finite::class("[a-c]", &['a', 'b', 'c']),
        Finite::class("[α-γ]", &['α', 'β', 'γ']),
        Finite::class(r"[\]x]", &[']', 'x']),
        Finite::class(r"[\-x]", &['-', 'x']),
        Finite::class("[.]", &['.']),
    ]);
    let mut cases = atoms.clone();
    for a in &atoms[..6] {
        for b in &atoms[9..15] {
            cases.push(a.then(b));
        }
    }
    for pair in atoms.windows(2) {
        cases.push(pair[0].either(&pair[1]));
    }
    for atom in &atoms {
        cases.push(Finite::literal("p/").then(atom));
    }
    cases.push(atoms[0].either(&atoms[9].either(&atoms[11])));
    cases.push(
        Finite::literal("p/")
            .either(&Finite::literal("q/"))
            .then(&atoms[9].either(&atoms[11])),
    );
    cases.push(Finite::class("[.]", &['.']).then(&Finite::class("[.]", &['.'])));
    cases
}

#[test]
fn constructive_escaped_unicode_brace_and_range_languages_intersect_exactly() {
    let examples = examples();
    let compiled: Vec<_> = examples
        .iter()
        .map(|e| Glob::compile(&e.source).unwrap())
        .collect();
    let languages: Vec<BTreeSet<_>> = examples
        .iter()
        .map(|e| e.words.iter().filter(|s| normalized(s)).cloned().collect())
        .collect();
    let universe: BTreeSet<_> = examples
        .iter()
        .flat_map(|e| e.words.iter().cloned())
        .chain([
            String::new(),
            "/a".into(),
            "p//a".into(),
            "p/../a".into(),
            "p/./a".into(),
            "a\0b".into(),
        ])
        .collect();
    for (i, glob) in compiled.iter().enumerate() {
        for word in &universe {
            assert_eq!(
                glob.is_match(word),
                languages[i].contains(word),
                "{:?} on {word:?}",
                examples[i].source
            );
        }
        for j in i..compiled.len() {
            let expected: BTreeSet<_> = languages[i].intersection(&languages[j]).cloned().collect();
            let witness = glob.overlap(&compiled[j]).unwrap();
            assert_eq!(
                witness.is_some(),
                !expected.is_empty(),
                "{:?} versus {:?}",
                examples[i].source,
                examples[j].source
            );
            assert_eq!(
                compiled[j].overlap(glob).unwrap().is_some(),
                !expected.is_empty()
            );
            if let Some(path) = witness {
                assert!(normalized(&path) && expected.contains(&path), "{path:?}");
            }
        }
    }
    println!(
        "FINITE_GLOB_ORACLE patterns={} pairs={} candidate_paths={}",
        examples.len(),
        examples.len() * (examples.len() + 1) / 2,
        universe.len()
    );
}

#[test]
fn infinite_unicode_wildcards_are_checked_against_complete_finite_languages() {
    type Predicate = fn(&str) -> bool;
    let cases: &[(&str, Predicate)] = &[
        ("*", |s| !s.contains('/')),
        ("?", |s| !s.contains('/') && s.chars().count() == 1),
        ("??", |s| !s.contains('/') && s.chars().count() == 2),
        ("[!a]", |s| {
            !s.contains('/') && s.chars().count() == 1 && s != "a"
        }),
        ("[^🦀]", |s| {
            !s.contains('/') && s.chars().count() == 1 && s != "🦀"
        }),
        ("*é*", |s| !s.contains('/') && s.contains('é')),
        ("**/é", |s| s.rsplit('/').next() == Some("é")),
        ("p/", |s| s == "p" || s.starts_with("p/")),
        ("**/**", |_| true),
    ];
    for (source, predicate) in cases {
        let infinite = Glob::compile(source).unwrap();
        for example in examples() {
            let finite = Glob::compile(&example.source).unwrap();
            let expected: BTreeSet<_> = example
                .words
                .iter()
                .filter(|s| normalized(s) && predicate(s))
                .cloned()
                .collect();
            let witness = infinite.overlap(&finite).unwrap();
            assert_eq!(
                witness.is_some(),
                !expected.is_empty(),
                "{source:?} versus {:?}",
                example.source
            );
            if let Some(path) = witness {
                assert!(expected.contains(&path) && normalized(&path));
            }
        }
    }
}

#[test]
fn utf8_byte_and_product_budgets_have_exact_boundaries() {
    for word in ["a".repeat(1_024), "é".repeat(512), "🦀".repeat(256)] {
        assert_eq!(word.len(), 1_024);
        let glob = Glob::compile(&word).unwrap();
        assert!(glob.is_match(&word));
        assert_eq!(
            Glob::compile(&format!("{word}x")).unwrap_err().code,
            ErrorCode::PatternTooComplex
        );
    }
    for word in ["a", "é", "🦀"] {
        let glob = Glob::compile(word).unwrap();
        assert_eq!(
            glob.overlap_with_limit(&glob, word.len() + 1)
                .unwrap()
                .as_deref(),
            Some(word)
        );
        assert_eq!(
            glob.overlap_with_limit(&glob, word.len()).unwrap_err().code,
            ErrorCode::PatternTooComplex
        );
    }
    let a = Glob::compile("a").unwrap();
    let b = Glob::compile("b").unwrap();
    assert!(a.overlap_with_limit(&b, 1).unwrap().is_none());
}

#[test]
fn actual_determinization_and_default_product_limits_fail_closed() {
    // Remembering a literal at a fixed distance from the end needs many DFA
    // states despite the small source. This exercises the real compiler cap,
    // independently of the source-byte and brace-expansion limits.
    let hard = format!("*a{}", "[ab]".repeat(20));
    assert_eq!(
        Glob::compile(&hard).unwrap_err().code,
        ErrorCode::PatternTooComplex
    );

    // Each component compiles below its own cap. They require different letters
    // at the same position from the end, so the languages are disjoint, but
    // deciding that product exceeds the ordinary (not test-reduced) budget.
    let a = Glob::compile(&format!("*a{}", "[abc]".repeat(11))).unwrap();
    let b = Glob::compile(&format!("*b{}", "[abc]".repeat(11))).unwrap();
    assert_eq!(
        a.overlap(&b).unwrap_err().code,
        ErrorCode::PatternTooComplex
    );
}

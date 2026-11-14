//! Historical corpus with the explicitly selected mention policy applied.
use agentchat_core::mentions;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
struct Case {
    id: String,
    source: String,
    expected: Value,
}

#[test]
fn accepted_270_case_corpus_matches_the_product_extractor() {
    // Source corpus f93c1678… from the retained second consumer evaluation.
    // Expectations preserve legacy outcomes except selected image/emoji/footnote
    // positives, authored image alt, and explicit depth-overflow refusal.
    let cases: Vec<Case> =
        serde_json::from_str(include_str!("fixtures/mde_compatibility_corpus.json")).unwrap();
    assert_eq!(cases.len(), 270);
    for case in cases {
        let actual = match mentions::tokens(&case.source) {
            Ok(tokens) => json!({"tokens": tokens}),
            Err(error) => json!({"error": error.code, "details": error.details}),
        };
        assert_eq!(actual, case.expected, "{}: {}", case.id, case.source);
    }
}

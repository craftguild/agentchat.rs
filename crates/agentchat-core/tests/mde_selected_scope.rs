//! User-selected normal acceptance cases; retain their exact corpus identities.
use agentchat_core::mentions;
use serde::Deserialize;
#[derive(Deserialize)]
struct Case {
    id: String,
    source: String,
    tokens: Vec<String>,
}
fn accepted(id: &str) {
    let cases: Vec<Case> =
        serde_json::from_str(include_str!("fixtures/mde_selected_scope.json")).unwrap();
    assert_eq!(cases.len(), 26);
    let case = cases.iter().find(|c| c.id == id).unwrap();
    let actual = mentions::tokens(&case.source)
        .unwrap()
        .into_iter()
        .collect::<Vec<_>>();
    assert_eq!(actual, case.tokens, "{}: {}", case.id, case.source);
}

#[test]
fn case_used_note() {
    accepted("case_used_note");
}

#[test]
fn case_emoji_boundary() {
    accepted("case_emoji_boundary");
}

#[test]
fn generated_208() {
    accepted("generated_208");
}

#[test]
fn generated_209() {
    accepted("generated_209");
}

#[test]
fn generated_210() {
    accepted("generated_210");
}

#[test]
fn generated_211() {
    accepted("generated_211");
}

#[test]
fn generated_212() {
    accepted("generated_212");
}

#[test]
fn generated_213() {
    accepted("generated_213");
}

#[test]
fn generated_214() {
    accepted("generated_214");
}

#[test]
fn generated_215() {
    accepted("generated_215");
}

#[test]
fn generated_216() {
    accepted("generated_216");
}

#[test]
fn generated_217() {
    accepted("generated_217");
}

#[test]
fn generated_218() {
    accepted("generated_218");
}

#[test]
fn generated_219() {
    accepted("generated_219");
}

#[test]
fn generated_223() {
    accepted("generated_223");
}

#[test]
fn generated_224() {
    accepted("generated_224");
}

#[test]
fn generated_225() {
    accepted("generated_225");
}

#[test]
fn generated_226() {
    accepted("generated_226");
}

#[test]
fn generated_227() {
    accepted("generated_227");
}

#[test]
fn generated_228() {
    accepted("generated_228");
}

#[test]
fn generated_229() {
    accepted("generated_229");
}

#[test]
fn generated_230() {
    accepted("generated_230");
}

#[test]
fn generated_231() {
    accepted("generated_231");
}

#[test]
fn generated_232() {
    accepted("generated_232");
}

#[test]
fn generated_233() {
    accepted("generated_233");
}

#[test]
fn generated_234() {
    accepted("generated_234");
}

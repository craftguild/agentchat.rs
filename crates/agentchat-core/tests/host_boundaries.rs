//! Executable positive/negative Rust syntax cases for the ADR0022 checker.
#[path = "../../../tools/host-boundary/policy.rs"]
mod policy;

#[test]
fn concrete_host_variants_and_label_dispatch_fail() {
    for source in [
        "fn f(h: HostKind) {if h == HostKind::Codex {send();}}",
        "fn f(h: HostKind) {match h {crate::model::HostKind::Claude => send(), _ => ()}}",
        "use crate::model::HostKind as Brand; fn f(h:Brand) {if h != Brand::Claude {send();}}",
        "use crate::model::HostKind::{self as Brand}; fn f(h:Brand) {if h==Brand::Codex {send();}}",
        "use crate::model::HostKind::{Codex as Native}; fn f(h:HostKind) {if h == Native {send();}}",
        "use crate::model::HostKind::*; fn f(h:HostKind) {if matches!(h, Codex) {send();}}",
        "use crate::model::HostKind as Brand; type Other=Brand; fn f(h:Other) {if h == Other::Codex {send();}}",
        "fn f(h:HostKind) {if matches!(h, HostKind::Claude) {send();}}",
        "fn f(label:&str) {if label == \"codex\" {send();}}",
        "fn f(label:&str) {match label {\"claude\" => send(), _ => ()}}",
        "const BRAND:&str=\"codex\"; fn f(label:&str) {if label != BRAND {send();}}",
        "fn f(label:&str) {let brand=\"claude\"; if label==brand {send();}}",
        "fn f(label:&str) {if matches!(label, \"codex\" | \"claude\") {send();}}",
        "fn f(label:&str) {if matches!(label, _ if label==\"codex\") {send();}}",
        "fn f(label:&str) {if label.eq_ignore_ascii_case(\"Claude\") {send();}}",
        "fn f(label:&str) {if [\"codex\",\"claude\"].contains(&label) {send();}}",
        "fn f(label:&str) {let brands=[\"codex\"]; if brands.contains(&label) {send();}}",
    ] {
        assert!(
            policy::check(source, false).is_err(),
            "Escaped boundary: {source}"
        );
    }
}

#[test]
fn host_label_constant_patterns_fail() {
    let mut escaped = Vec::new();
    for label in ["codex", "claude"] {
        for (declaration, pattern) in [
            (format!("const BRAND:&str=\"{label}\";"), "BRAND"),
            (
                format!("const ALIAS:&str=BRAND; const BRAND:&str=\"{label}\";"),
                "ALIAS",
            ),
            (
                format!("mod labels {{pub const BRAND:&str=\"{label}\";}}"),
                "crate::labels::BRAND",
            ),
            (
                format!(
                    "use crate::labels::BRAND as NATIVE; mod labels {{pub const BRAND:&str=\"{label}\";}}"
                ),
                "NATIVE",
            ),
            (
                format!("struct Labels; impl Labels {{const BRAND:&str=\"{label}\";}}"),
                "Labels::BRAND",
            ),
        ] {
            for body in [
                format!("match host {{{pattern}=>true,_=>false}}"),
                format!("matches!(host, {pattern})"),
                format!("std::matches!(host, {pattern})"),
                format!("if let {pattern}=host {{true}} else {{false}}"),
                format!("while let {pattern}=host {{return true;}} false"),
                format!("let {pattern}=host else {{return false;}}; true"),
                format!("match Some(host) {{Some({pattern})=>true,_=>false}}"),
                format!("match &host {{&({pattern} | \"other\")=>true,_=>false}}"),
            ] {
                let source = format!("{declaration} fn choose(host:&str)->bool {{{body}}}");
                if policy::check(&source, false).is_ok() {
                    escaped.push(source);
                }
            }
        }
        let source = format!(
            "fn choose(host:&str)->bool {{const BRAND:&str=\"{label}\"; matches!(host, BRAND)}}"
        );
        if policy::check(&source, false).is_ok() {
            escaped.push(source);
        }
    }
    assert!(
        escaped.is_empty(),
        "Escaped boundary:\n{}",
        escaped.join("\n")
    );
}

#[test]
fn reexported_label_constant_patterns_fail_across_sources() {
    for decision in [
        "match host {PUBLIC=>true,_=>false}",
        "matches!(host, PUBLIC)",
    ] {
        assert!(
            policy::check_files(&[
                (
                    "policy.rs".into(),
                    format!("use crate::identity::EXPORTED as PUBLIC; fn choose(host:&str)->bool {{{decision}}}"),
                    false,
                ),
                (
                    "identity.rs".into(),
                    "pub use crate::labels::BRAND as EXPORTED;".into(),
                    false,
                ),
                (
                    "labels.rs".into(),
                    "pub const BRAND:&str=\"codex\";".into(),
                    false,
                ),
            ])
            .is_err(),
            "Escaped boundary: {decision}"
        );
    }
}

#[test]
fn reexported_aliases_are_resolved_across_sources() {
    assert!(
        policy::check_files(&[
            (
                "policy.rs".into(),
                "use crate::identity::Brand; fn f(h:Brand) {if h==Brand::Codex {send();}}".into(),
                false
            ),
            (
                "identity.rs".into(),
                "pub use crate::model::HostKind as Brand;".into(),
                false
            ),
        ])
        .is_err()
    );
}

#[test]
fn external_and_conditional_paths_cannot_hide_policy_in_test_or_generated_files() {
    for attribute in [
        "path=\"../tests/hidden.rs\"",
        "cfg_attr(test,path=\"generated/hidden.rs\")",
    ] {
        assert!(policy::check(&format!("#[{attribute}] mod hidden;"), false).is_err());
    }
    assert!(
        policy::check_files(&[
            (
                "src/delivery.rs".into(),
                "#[path=\"delivery/input.rs\"] mod input;".into(),
                false
            ),
            (
                "src/delivery/input.rs".into(),
                "fn input() {}".into(),
                false
            ),
        ])
        .is_ok()
    );
    assert!(
        policy::check_files(&[
            (
                "src/policy.rs".into(),
                "#[path=\"../../src/hidden.rs\"] mod hidden;".into(),
                false
            ),
            ("src/hidden.rs".into(), "fn hidden() {}".into(), false),
        ])
        .is_err()
    );
}

#[test]
fn comments_diagnostics_identity_values_and_common_mechanisms_pass() {
    for source in [
        "// if host == HostKind::Codex {}\nfn f() {}",
        "fn f() {let description=\"HostKind::Codex == codex\";}",
        "fn f() {let label=\"codex\"; log(label);}",
        "const BRAND:&str=\"codex\"; fn f() {log(BRAND);}",
        "mod labels {pub const BRAND:&str=\"claude\";} use crate::labels::BRAND as NATIVE; fn f() {log(NATIVE);}",
        "const BRAND:&str=\"other\"; fn f(label:&str)->bool {matches!(label, BRAND)}",
        "fn f(host:&str)->bool {matches!(host, binding if binding.is_empty())}",
        "fn f(host:&str)->bool {let label=\"codex\"; log(label); matches!(host, label if label.is_empty())}",
        "use crate::model::HostKind as Brand; fn f(h:Brand) {save_identity(h);}",
        "fn f(mode:InputRecovery) {match mode {InputRecovery::OriginalReceipt=>poll(), _=>hold()}}",
        "fn f() {json!({\"host\":\"claude\"});}",
        "fn f() {let ordinary_text=include_str!(\"text.txt\"); log(ordinary_text);}",
    ] {
        assert!(
            policy::check(source, false).is_ok(),
            "False positive: {source}"
        );
    }
}

#[test]
fn identity_allowance_is_structural_and_narrow() {
    let encoding = "impl HostKind {pub const fn as_str(self)->&'static str {match self {Self::Codex=>\"codex\",Self::Claude=>\"claude\"}}}";
    assert!(policy::check(encoding, true).is_ok());
    let bad = encoding.replace("\"codex\"", "select_policy()");
    assert!(policy::check(&bad, true).is_err());
}

#[test]
fn block_and_trait_default_constants_fail() {
    for label in ["codex", "claude"] {
        for (declaration, pattern) in [
            (format!("const BRAND:&str={{\"{label}\"}};"), "BRAND"),
            (
                format!("const BRAND:&str={{{{ALIAS}}}}; const ALIAS:&str=\"{label}\";"),
                "BRAND",
            ),
            (
                format!(
                    "trait Labels {{const BRAND:&'static str={{\"{label}\"}};}} struct Tag; impl Labels for Tag {{}}"
                ),
                "<Tag as Labels>::BRAND",
            ),
            (
                format!(
                    "trait Labels {{const SOURCE:&'static str=\"{label}\"; const BRAND:&'static str={{Self::SOURCE}};}} struct Tag; impl Labels for Tag {{}}"
                ),
                "<Tag as Labels>::BRAND",
            ),
            (
                format!(
                    "mod labels {{pub trait Labels {{const BRAND:&'static str=\"{label}\";}}}} use labels::Labels as Names; struct Tag; impl Names for Tag {{}}"
                ),
                "<Tag as Names>::BRAND",
            ),
        ] {
            for body in [
                format!("match host {{{pattern}=>true,_=>false}}"),
                format!("matches!(host, {pattern})"),
                format!("if let {pattern}=host {{true}} else {{false}}"),
            ] {
                let source = format!("{declaration} fn choose(host:&str)->bool {{{body}}}");
                assert!(policy::check(&source, false).is_err(), "Escaped: {source}");
            }
        }
    }
}

#[test]
fn unrelated_same_name_constants_pass() {
    for decision in [
        "matches!(value, NAME)",
        "match value {self::NAME=>true,_=>false}",
        "matches!(value, crate::operations::NAME)",
    ] {
        let source = format!(
            "mod diagnostics {{pub const NAME:&str=\"codex\";}} mod operations {{const NAME:&str={{\"ordinary\"}}; pub fn choose(value:&str)->bool {{{decision}}}}}"
        );
        assert!(
            policy::check(&source, false).is_ok(),
            "False positive: {source}"
        );
    }
    for source in [
        "const NAME:&str=\"codex\"; mod operations {const NAME:&str=\"ordinary\"; pub fn f(h:&str)->bool {matches!(h, NAME)}}",
        "mod diagnostics {pub const NAME:&str=\"codex\";} mod values {pub const NAME:&str=\"ordinary\";} use values::NAME; fn f(h:&str)->bool {matches!(h, NAME)}",
        "mod diagnostics {pub const NAME:&str=\"codex\";} mod values {pub const NAME:&str=\"ordinary\";} use values as operations; fn f(h:&str)->bool {matches!(h, operations::NAME)}",
        "mod diagnostics {pub const NAME:&str=\"codex\";} mod values {pub const NAME:&str=\"ordinary\";} use diagnostics::*; use values::NAME; fn f(h:&str)->bool {matches!(h, NAME)}",
        "fn diagnostic() {const NAME:&str=\"codex\"; log(NAME);} fn f(h:&str)->bool {const NAME:&str=\"ordinary\"; matches!(h, NAME)}",
        "const NAME:&str=\"codex\"; fn f(h:&str)->bool {const NAME:&str=\"ordinary\"; matches!(h, NAME)}",
        "struct A; impl A {const NAME:&str=\"codex\";} struct B; impl B {const NAME:&str=\"ordinary\";} fn f(h:&str)->bool {matches!(h, B::NAME)}",
        "trait A {const NAME:&'static str=\"codex\";} trait B {const NAME:&'static str=\"ordinary\";} struct Tag; impl A for Tag {} impl B for Tag {} fn f(h:&str)->bool {matches!(h, <Tag as B>::NAME)}",
        "trait A {const NAME:&'static str=\"codex\";} struct Tag; impl A for Tag {const NAME:&'static str=\"ordinary\";} fn f(h:&str)->bool {matches!(h, <Tag as A>::NAME)}",
        "trait A {const NAME:&'static str=\"ordinary\";} struct Tag; impl A for Tag {} fn f(h:&str)->bool {matches!(h, <Tag as A>::NAME)}",
        "trait A {const SOURCE:&'static str=\"codex\"; const NAME:&'static str={Self::SOURCE};} struct Tag; impl A for Tag {const SOURCE:&'static str=\"ordinary\";} fn f(h:&str)->bool {matches!(h, <Tag as A>::NAME)}",
        "fn diagnostic() {let name=\"codex\"; log(name);} fn f(h:&str)->bool {let name=\"ordinary\"; h==name}",
        "const NAME:&str=\"ordinary\"; struct NAME {value:usize} impl NAME {const LABEL:&str=\"codex\";} fn f(h:&str)->bool {matches!(h, NAME)}",
        "const NAME:&str=\"codex\"; struct NAME {value:usize} impl NAME {const LABEL:&str=\"ordinary\";} fn f(h:&str)->bool {matches!(h, NAME::LABEL)}",
        "const ALIAS:&str=\"ordinary\"; struct Other {value:usize} const Other:&str=\"codex\"; fn f(h:&str)->bool {type ALIAS=Other; matches!(h, ALIAS)}",
    ] {
        assert!(
            policy::check(source, false).is_ok(),
            "False positive: {source}"
        );
    }
}

#[test]
fn scoped_constant_references_still_fail() {
    for (declaration, pattern) in [
        (
            "mod labels {pub const NAME:&str=\"codex\";} use labels as native;",
            "native::NAME",
        ),
        (
            "mod labels {pub const NAME:&str=\"codex\";} use labels::*;",
            "NAME",
        ),
        (
            "mod labels {pub const NAME:&str=\"codex\"; pub mod nested {pub const ALIAS:&str=super::NAME;}}",
            "labels::nested::ALIAS",
        ),
        (
            "mod labels {pub const NAME:&str=\"codex\";} mod inner {use super::labels::{self}; pub fn f(h:&str)->bool {matches!(h, labels::NAME)}}",
            "labels::NAME",
        ),
        (
            "trait A {const NAME:&'static str=\"codex\";} struct Tag; impl A for Tag {}",
            "Tag::NAME",
        ),
        (
            "trait A {const NAME:&'static str=\"ordinary\";} impl A for String {const NAME:&'static str=\"codex\";}",
            "<String as A>::NAME",
        ),
        (
            "use std::string::String as Text; trait A {const NAME:&'static str=\"ordinary\";} impl A for Text {const NAME:&'static str=\"codex\";}",
            "<Text as A>::NAME",
        ),
        (
            "trait A {const SOURCE:&'static str=\"ordinary\"; const NAME:&'static str={Self::SOURCE};} struct Tag; impl A for Tag {const SOURCE:&'static str=\"codex\";}",
            "<Tag as A>::NAME",
        ),
        (
            "trait A {const NAME:&'static str=\"ordinary\";} struct Tag; impl A for Tag {const NAME:&'static str=\"codex\";}",
            "<Tag as A>::NAME",
        ),
    ] {
        let source = format!("{declaration} fn f(h:&str)->bool {{matches!(h, {pattern})}}");
        assert!(policy::check(&source, false).is_err(), "Escaped: {source}");
    }
    for source in [
        "const NAME:&str=\"codex\"; struct NAME {value:usize} fn f(h:&str)->bool {matches!(h, NAME)}",
        "struct NAME {value:usize} const NAME:&str=\"codex\"; fn f(h:&str)->bool {matches!(h, NAME)}",
        "const NAME:&str=\"ordinary\"; struct NAME {value:usize} impl NAME {const LABEL:&str=\"codex\";} fn f(h:&str)->bool {matches!(h, NAME::LABEL)}",
        "mod names {pub const NAME:&str=\"codex\"; pub struct NAME {value:usize}} use names::NAME as BOTH; fn f(h:&str)->bool {matches!(h, BOTH)}",
        "const ALIAS:&str=\"codex\"; struct Other {value:usize} const Other:&str=\"ordinary\"; fn f(h:&str)->bool {type ALIAS=Other; matches!(h, ALIAS)}",
    ] {
        assert!(policy::check(source, false).is_err(), "Escaped: {source}");
    }
}

#[test]
fn cross_source_names_resolve_to_their_modules() {
    for (label, rejected) in [("ordinary", false), ("codex", true)] {
        let result = policy::check_files(&[
            (
                "src/nested/renamed.rs".into(),
                "use super::NAME; pub fn f(h:&str)->bool {matches!(h, NAME)}".into(),
                false,
            ),
            (
                "src/lib.rs".into(),
                format!(
                    "pub const NAME:&str=\"{label}\"; #[path=\"nested/renamed.rs\"] mod operations;"
                ),
                false,
            ),
        ]);
        assert_eq!(
            result.is_err(),
            rejected,
            "Explicit module path: {label}: {result:?}"
        );
    }
    for (reference, rejected) in [
        ("ordinary::NAME", false),
        ("native::NAME", true),
        ("NAME", false),
    ] {
        let result = policy::check_files(&[
            (
                "src/lib.rs".into(),
                format!(
                    "mod diagnostics; mod operations; use operations as ordinary; use diagnostics as native; use operations::NAME; fn f(h:&str)->bool {{matches!(h, {reference})}}"
                ),
                false,
            ),
            (
                "src/diagnostics.rs".into(),
                "pub const NAME:&str=\"codex\";".into(),
                false,
            ),
            (
                "src/operations.rs".into(),
                "pub const NAME:&str=\"ordinary\"; pub fn f(h:&str)->bool {matches!(h, NAME)}"
                    .into(),
                false,
            ),
        ]);
        assert_eq!(result.is_err(), rejected, "{reference}: {result:?}");
    }
}

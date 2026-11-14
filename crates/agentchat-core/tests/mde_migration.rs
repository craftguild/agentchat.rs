//! Product migration qualification: consumer policy and parsing availability.
use agentchat_core::{ErrorCode, mentions};
use oriha_core::{EngineLimits, OrihaEngine, ParseStatus, RenderInput};

#[test]
fn selected_delivery_policy_and_provenance() {
    for (source, expected) in [
        (
            r"@Alice \@Alice \\@Bob &#64;Carol @ali**ce**",
            vec!["alice", "bob", "carol"],
        ),
        ("![hello @alt](https://x/@destination)", vec!["alt"]),
        (
            "main[^used]\n\n[^unused]: @hidden\n\n[^used]: @note",
            vec!["note"],
        ),
        ("word:rocket:@edge", vec!["edge"]),
        ("<i title='@attribute'>@visible</i>", vec!["visible"]),
        (
            "`@code`\n\n```text\n@fenced\n```\n\n@all @here @channel",
            vec!["all", "channel", "here"],
        ),
    ] {
        assert_eq!(
            mentions::tokens(source)
                .unwrap()
                .into_iter()
                .collect::<Vec<_>>(),
            expected,
            "{source}"
        );
    }
}

#[test]
fn empty_complete_is_distinct_from_incomplete_and_failed() {
    assert!(mentions::tokens("ordinary prose").unwrap().is_empty());
    for (source, limits, status) in [
        (
            "@first\n\n@last",
            EngineLimits {
                max_blocks: 1,
                ..Default::default()
            },
            ParseStatus::Incomplete,
        ),
        (
            "> @hidden\n",
            EngineLimits {
                max_nesting_depth: 0,
                ..Default::default()
            },
            ParseStatus::Incomplete,
        ),
        (
            "@hidden",
            EngineLimits {
                max_source_bytes: 1,
                ..Default::default()
            },
            ParseStatus::Failed,
        ),
    ] {
        let parsed = OrihaEngine::default()
            .with_features(mentions::markdown_features())
            .with_limits(limits)
            .parse(RenderInput {
                markdown: source.into(),
                ..Default::default()
            });
        assert_eq!(parsed.parse_status(), status);
        for e in [
            mentions::occurrences(&parsed).unwrap_err(),
            mentions::delivery_tokens(&parsed).unwrap_err(),
        ] {
            assert_eq!(e.code, ErrorCode::InvalidArgument);
            assert!(e.details["markdown_parse_status"].is_string());
        }
    }
}

#[test]
fn routing_budget_covers_wide_input_that_exceeds_the_viewer_budget() {
    let source = format!("{}@tail", "# heading\n".repeat(6000));
    assert!(source.len() <= 65536);
    assert_eq!(
        mentions::tokens(&source)
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>(),
        ["tail"]
    );
}

#[test]
fn deep_routing_is_explicitly_unavailable_without_partial_candidates() {
    let source = format!("{}@hidden\n", "> ".repeat(66));
    let e = mentions::tokens(&source).unwrap_err();
    assert_eq!(e.details["markdown_parse_status"], "incomplete");
}

#[test]
fn quote_and_mixed_container_depth_follow_the_shared_limit() {
    for depth in [63, 64] {
        let source = format!("{}@edge\n", "> ".repeat(depth));
        assert!(mentions::tokens(&source).unwrap().contains("edge"));
    }
    for source in [
        format!("{}@hidden\n", "> ".repeat(65)),
        format!("{}- @hidden\n", "> ".repeat(64)),
    ] {
        let error = mentions::tokens(&source).unwrap_err();
        assert_eq!(error.details["markdown_parse_status"], "incomplete");
    }
}

#[test]
fn maximum_bytes_and_many_distinct_mentions_obey_existing_bounds() {
    let source = format!("{} @tail", "a".repeat(65530));
    assert_eq!(source.len(), 65536);
    assert!(mentions::tokens(&source).unwrap().contains("tail"));
    assert!(mentions::tokens(&"a".repeat(65537)).is_err());
    let source = (0..201).map(|n| format!("@person{n} ")).collect::<String>();
    let e = mentions::resolve(&source, uuid::Uuid::new_v4(), &[]).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
}

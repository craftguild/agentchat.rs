//! Delivery-only public-IR alt extraction with common provenance and exclusions.
use agentchat_core::mentions;
use oriha_core::{
    OrihaEngine, RenderHook, RenderInput,
    ir::{BlockKind, Document, InlineKind, InlineNode},
    render::{RenderContext, RenderState},
};

#[test]
fn image_alt_uses_the_common_lexer_without_visible_region_authority() {
    for (source, expected) in [
        ("![description @Bob](url)", vec!["bob"]),
        (r"![\@bob](url)", vec![]),
        (r"![\\@Bob](url)", vec!["bob"]),
        ("![&commat;Bob](url)", vec!["bob"]),
        ("![&#64;bo**b**](url)", vec!["bob"]),
        ("![word&colon;@bob](url)", vec![]),
        ("![word:rocket:@bob](url)", vec!["bob"]),
        (
            "![`@code` @bob](https://x/@destination \"@title\")",
            vec!["bob"],
        ),
        ("> - ![日本語 &commat;BOB](url)\r\n", vec!["bob"]),
        ("| x |\n|---|\n| ![hello @bob](url) |", vec!["bob"]),
        ("main[^n]\n\n[^n]: ![description @bob](url)", vec!["bob"]),
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
    let parsed = mentions::parse("![description @bob](url)").unwrap();
    assert!(mentions::occurrences(&parsed).unwrap().is_empty());
    assert!(mentions::delivery_tokens(&parsed).unwrap().contains("bob"));
}

#[test]
fn excluded_contexts_do_not_select_alternatives_or_retained_footnotes() {
    for source in [
        "`![description @bob](url)`",
        "```md\n![description @bob](url)\n```",
        "[label](https://x/@bob \"@bob\")",
        "<img alt='@bob' src='url'>",
        "ordinary\n\n[^unused]: ![description @bob](url)",
        "main[^one]\n\n[^one]: nested[^two]\n\n[^two]: ![description @bob](url)",
        "![description[^note]](url)\n\n[^note]: @bob",
        "![description &#+64;bob &#x+40;bob &#000000064;bob &#x00000040;bob](url)",
    ] {
        assert!(mentions::tokens(source).unwrap().is_empty(), "{source}");
    }
}

#[test]
fn generated_nodes_cannot_complete_an_original_at_sign() {
    struct GeneratedSuffix;
    impl RenderHook for GeneratedSuffix {
        fn after_document_parse(
            &self,
            document: &mut Document,
            _: &mut RenderState,
            _: &mut RenderContext,
        ) {
            let BlockKind::Paragraph(paragraph) = &mut document.blocks[0].kind else {
                panic!("paragraph")
            };
            for node in &mut paragraph.children {
                if let InlineKind::Image { alt, .. } = &mut node.kind {
                    alt.push(InlineNode::generated(InlineKind::Text("bob".into())));
                }
            }
            paragraph
                .children
                .push(InlineNode::generated(InlineKind::Text(" @mallory".into())));
        }
    }
    let parsed = OrihaEngine::default()
        .with_features(mentions::markdown_features())
        .add_hook(GeneratedSuffix)
        .parse(RenderInput {
            markdown: "![description @](url)".into(),
            ..Default::default()
        });
    assert!(mentions::delivery_tokens(&parsed).unwrap().is_empty());
}

#[test]
fn image_alternatives_participate_in_the_existing_distinct_handle_bound() {
    let names = (0..201).map(|i| format!("@person{i} ")).collect::<String>();
    let body = format!("![{names}](url)");
    assert!(mentions::resolve(&body, uuid::Uuid::new_v4(), &[]).is_err());
    assert_eq!(
        mentions::tokens("![description @bob](url) @BOB")
            .unwrap()
            .len(),
        1
    );
}

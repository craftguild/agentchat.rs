//! Real migrated annotation/sanitizer behavior, not an isolated engine example.
use agentchat_web::render::Renderer;

#[tokio::test]
async fn shared_policy_decorates_visible_footnotes_but_not_image_alt_or_source_attributes() {
    let source = "main[^note]\n\n[^unused]: @hidden\n\n[^note]: @ali**ce**\n\n![image @alt](https://x/)\n\n<i title='@attribute'>@visible</i>";
    let result = Renderer::default().render(source).await;
    assert!(!result.fallback);
    assert_eq!(result.html.matches("data-mention=\"alice\"").count(), 2);
    assert_eq!(result.html.matches("data-mention=\"visible\"").count(), 1);
    for hidden in ["hidden", "alt", "attribute"] {
        assert!(!result.html.contains(&format!("data-mention=\"{hidden}\"")));
    }
    assert!(!result.html.contains("data-mde-annotation"));
    assert!(!result.html.contains("evaluation-owned"));
}

#[tokio::test]
async fn incomplete_viewer_parsing_uses_the_entire_original_fallback() {
    let source = format!("{}@tail", "# heading\n".repeat(6000));
    let result = Renderer::default().render(&source).await;
    assert!(result.fallback);
    assert_eq!(
        result.html,
        format!("<pre class=\"markdown-fallback\">{source}</pre>")
    );
    assert!(result.html.contains("@tail"));
}

#[tokio::test]
async fn necessarily_oversized_annotation_plans_preserve_complete_plain_and_formatted_bodies() {
    for source in ["@all ".repeat(12_000), "@ali**ce** ".repeat(5_000)] {
        assert!(source.len() <= 65_536);
        let result = Renderer::default().render(&source).await;
        assert!(result.fallback);
        assert_eq!(
            result.html,
            format!("<pre class=\"markdown-fallback\">{source}</pre>")
        );
    }
}

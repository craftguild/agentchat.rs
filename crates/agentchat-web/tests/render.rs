//! Real renderer/sanitizer behavior, complete fallback and cache budget.

use agentchat_web::render::{MAX_CACHE_BYTES, Renderer};
use std::sync::Arc;

#[tokio::test]
async fn gfm_highlighting_and_semantic_html_work_without_active_markdown_features() {
    let renderer = Renderer::default();
    let source = "# Heading\n\n~~removed~~\n\n| A | B |\n|---|---|\n| x | y |\n\n- [x] done\n\n```rust\nfn main() {}\n```\n\n:::note\ntext\n:::\n\n$math$\n\n```mermaid\ngraph TD; A-->B\n```";
    let body = renderer.render(source).await;
    assert!(!body.fallback);
    assert!(body.html.contains("<h1>Heading</h1>"));
    assert!(body.html.contains("<table"));
    assert!(body.html.contains("<del>") || body.html.contains("<s>"));
    assert!(body.html.contains("<code"));
    assert!(!body.html.contains("<svg"));
    assert!(!body.html.contains("<style"));
    assert!(!body.html.contains("callout-"));
}

#[tokio::test]
async fn hostile_markdown_cannot_supply_executable_elements_urls_attributes_or_resources() {
    let renderer = Renderer::default();
    let source = "<script>globalThis.pwned=1</script>\n\n<img src=x onerror=alert(1)>\n\n[bad](javascript:alert(1))\n\n[data](data:text/html,evil)\n\n[good](https://example.invalid/)\n\n![tracking](https://example.invalid/pixel)\n\n<iframe srcdoc=evil></iframe>\n\n<svg onload=alert(1)></svg>";
    let body = renderer.render(source).await;
    assert!(!body.fallback);
    for forbidden in [
        "<script",
        "<img",
        "<iframe",
        "<svg",
        "href=\"javascript:",
        "href=\"data:",
    ] {
        assert!(!body.html.contains(forbidden), "{forbidden}: {}", body.html);
    }
    assert!(body.html.contains("href=\"https://example.invalid/\""));
    assert!(body.html.contains("noopener noreferrer nofollow"));
}

#[tokio::test]
async fn bounded_engine_failure_displays_the_complete_escaped_original() {
    let renderer = Renderer::default();
    let source = "# <&>\n".repeat(9000);
    assert!(source.len() <= 65_536);
    let body = renderer.render(&source).await;
    assert!(body.fallback);
    assert_eq!(
        body.html,
        format!(
            "<pre class=\"markdown-fallback\">{}</pre>",
            source
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
        )
    );
}

#[tokio::test]
async fn maximum_unicode_and_expanding_bodies_survive_and_identical_bodies_share_cache() {
    let renderer = Renderer::default();
    for source in ["💜".repeat(16_384), "\0".repeat(65_536), "&".repeat(65_536)] {
        let first = renderer.render(&source).await;
        let second = renderer.render(&source).await;
        assert!(Arc::ptr_eq(&first, &second));
        assert!(first.html.len() < 512 * 1024);
        assert_eq!(source.len(), 65_536);
    }
    assert!(renderer.cached_bytes() <= MAX_CACHE_BYTES);
}

#[tokio::test]
async fn cache_evicts_by_accounted_bytes_and_item_count() {
    let renderer = Renderer::default();
    for index in 0..300 {
        renderer
            .render(&format!("{index} {}", "&".repeat(8000)))
            .await;
        assert!(renderer.cached_bytes() <= MAX_CACHE_BYTES);
    }
    assert!(renderer.cached_bytes() > 0);
}

#[tokio::test]
async fn source_verified_mentions_preserve_escapes_formatting_and_safe_attributes() {
    let source = r"@Alice \@Alice @ali**ce** @all @unknown `@Alice` a@alice.test https://example.test/@Alice";
    let result = Renderer::default().render(source).await;
    assert!(!result.fallback);
    assert_eq!(result.html.matches("data-mention=\"alice\"").count(), 3);
    assert_eq!(result.html.matches("data-mention=\"all\"").count(), 1);
    assert_eq!(result.html.matches("data-mention=\"unknown\"").count(), 1);
    assert!(result.html.contains("<code>@Alice</code>"));
    assert!(
        result
            .html
            .contains("<strong><span class=\"mention\" data-mention=\"alice\">ce</span></strong>")
    );
    assert!(!result.html.contains("tabindex"));
    let hostile = Renderer::default()
        .render(r#"<span class="mention" data-mention="victim" tabindex="0">fake</span>"#)
        .await;
    // Oriha preserves disabled raw HTML as escaped text, including its attributes.
    assert!(hostile.html.contains("&lt;span"), "{}", hostile.html);
    assert!(!hostile.html.contains("<span"), "{}", hostile.html);
}

#[tokio::test]
async fn mention_decoration_supports_headings_lists_tables_links_and_unicode() {
    let source = "# @alice\n\n- 日本語 @alice\n\n> @alice\n\n| Who |\n|---|\n| @alice |\n\n[@alice](https://example.test/)\n\n```text\n@alice\n```";
    let result = Renderer::default().render(source).await;
    assert!(!result.fallback);
    assert_eq!(
        result.html.matches("data-mention=\"alice\"").count(),
        5,
        "{}",
        result.html
    );
    assert!(result.html.contains("<a href="));
}

#[tokio::test]
async fn mention_markup_expansion_obeys_the_existing_complete_fallback_budget() {
    let source = "@all ".repeat(12_000);
    let result = Renderer::default().render(&source).await;
    assert!(result.fallback);
    assert_eq!(
        result.html,
        format!("<pre class=\"markdown-fallback\">{source}</pre>")
    );
}

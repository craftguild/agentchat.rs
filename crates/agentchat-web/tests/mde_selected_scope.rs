//! Selected delivery scope and visible annotations share parsing, not target sets.
use agentchat_core::mentions;
use agentchat_web::render::Renderer;

#[tokio::test]
async fn all_26_selected_visible_cases_are_normal_decorated_rendering() {
    let cases: Vec<serde_json::Value> = serde_json::from_str(include_str!(
        "../../agentchat-core/tests/fixtures/mde_selected_scope.json"
    ))
    .unwrap();
    assert_eq!(cases.len(), 26);
    for case in cases {
        let body = case["source"].as_str().unwrap();
        let result = Renderer::default().render(body).await;
        assert!(!result.fallback, "{}: {body}", case["id"]);
        for name in case["tokens"].as_array().unwrap() {
            assert!(
                result
                    .html
                    .contains(&format!("data-mention=\"{}\"", name.as_str().unwrap())),
                "{}: {}",
                case["id"],
                result.html
            );
        }
    }
}

#[tokio::test]
async fn four_representatives_keep_delivery_separate_from_visible_annotation() {
    for (body, visible) in [
        ("![description @bob](url)", false),
        ("![alt](url)@bob", true),
        ("word:rocket:@bob", true),
        ("main[^n]\n\n[^n]: @bob", true),
    ] {
        assert_eq!(
            mentions::tokens(body)
                .unwrap()
                .into_iter()
                .collect::<Vec<_>>(),
            ["bob"]
        );
        let result = Renderer::default().render(body).await;
        assert!(!result.fallback);
        assert_eq!(
            result.html.contains("data-mention=\"bob\""),
            visible,
            "{body}: {}",
            result.html
        );
        if !visible {
            assert!(!result.html.contains("@bob"));
        }
        assert!(!result.html.contains("data-mde-annotation"));
    }
}

//! Manual report-only costs and resource scope through private product stages.
use super::{MAX_CACHE_BYTES, MAX_HTML_BYTES, Renderer, render, sanitize};
use agentchat_core::mentions;
use oriha_core::{EngineLimits, OrihaEngine, ParseStatus, RenderInput, RenderMode, Severity};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, hint::black_box, io::Write, sync::Arc, time::Instant};

const MAX_BYTES: usize = 65_536;
const WARMUPS: usize = 2;
const SAMPLES: usize = 5;
const RESOURCE_BODIES: usize = 256;

struct Recipe {
    name: &'static str,
    source: String,
    fallback: bool,
}

fn filled(bytes: usize, unit: &str, suffix: &str) -> String {
    let available = bytes.checked_sub(suffix.len()).unwrap();
    let mut body = unit.repeat(available / unit.len());
    body.push_str(&"x".repeat(available % unit.len()));
    body.push_str(suffix);
    assert_eq!(body.len(), bytes);
    body
}

fn recipes() -> Vec<Recipe> {
    let alt = filled(
        MAX_BYTES - "![".len() - "](image.png)".len(),
        "alt @bob ",
        "",
    );
    vec![
        Recipe {
            name: "typical_short",
            source: r"Hello @bo**b** \@escaped &commat;carol.".into(),
            fallback: false,
        },
        Recipe {
            name: "maximum_trailing_mention",
            source: filled(MAX_BYTES, "ordinary prose ", " @tail"),
            fallback: false,
        },
        Recipe {
            name: "maximum_multiline",
            source: filled(MAX_BYTES, "ordinary prose line\n", "\n\n@tail"),
            fallback: false,
        },
        Recipe {
            name: "formatted_dense",
            source: filled(
                55_000,
                "formatted **bold** and *emphasis* text ",
                "\n\n@ali**ce** @bob",
            ),
            fallback: false,
        },
        Recipe {
            name: "maximum_image_alt",
            source: format!("![{alt}](image.png)"),
            fallback: false,
        },
        Recipe {
            name: "many_blocks_complete_source_fallback",
            source: filled(MAX_BYTES, "# heading\n\n", "\n\n@tail"),
            fallback: true,
        },
        Recipe {
            name: "formatted_mentions_complete_source_fallback",
            source: "@ali**ce** ".repeat(5000),
            fallback: true,
        },
    ]
}

fn digest(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn summary(samples: Vec<u64>) -> Value {
    assert_eq!(samples.len(), SAMPLES);
    let mut ordered = samples.clone();
    ordered.sort_unstable();
    json!({"median_ns": ordered[SAMPLES / 2], "samples_ns": samples})
}

fn elapsed(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap()
}

fn measure<T>(mut operation: impl FnMut() -> T) -> Value {
    for _ in 0..WARMUPS {
        black_box(operation());
    }
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let started = Instant::now();
        let result = black_box(operation());
        samples.push(elapsed(started));
        black_box(result);
    }
    summary(samples)
}

fn input(source: &str) -> RenderInput {
    RenderInput {
        markdown: source.to_owned(),
        mode: RenderMode::SemanticHtml,
        features: mentions::markdown_features(),
        ..Default::default()
    }
}

fn stage_costs(source: &str) -> (Value, Option<String>) {
    // Mirror the current private render function's actual limits and profile.
    // The final staged output is compared with that function for every recipe.
    let engine = OrihaEngine::default()
        .with_features(mentions::markdown_features())
        .with_limits(EngineLimits {
            max_source_bytes: MAX_BYTES,
            max_blocks: 4096,
            max_nesting_depth: 64,
            max_output_bytes: MAX_HTML_BYTES,
        });
    let parse = measure(|| engine.parse(input(black_box(source))));
    let parsed = engine.parse(input(source));
    let mut stages = json!({
        "parse": parse,
        "parse_status": format!("{:?}", parsed.parse_status()).to_ascii_lowercase(),
        "parse_diagnostics": parsed.diagnostics().iter().map(|d| &d.code).collect::<Vec<_>>(),
        "annotation": null, "render_parsed": null, "sanitizer": null,
    });
    if parsed.parse_status() != ParseStatus::Complete {
        stages["stopped_at"] = json!("incomplete_or_failed_parse_full_source_fallback");
        return (stages, None);
    }
    let marker = format!("{}:", uuid::Uuid::new_v4().simple());
    stages["annotation"] =
        measure(|| crate::render_mentions::decorate(black_box(&parsed), &marker));
    let decorated = match crate::render_mentions::decorate(&parsed, &marker) {
        Ok(value) => value,
        Err(error) => {
            stages["stopped_at"] = json!("annotation_rejected_full_source_fallback");
            stages["annotation_error"] = json!(error.code);
            return (stages, None);
        }
    };
    stages["render_parsed"] = measure(|| engine.render_parsed(black_box(&decorated)));
    let output = engine.render_parsed(&decorated);
    stages["raw_html_bytes"] = json!(output.html.len());
    stages["render_diagnostics"] = json!(
        output
            .diagnostics
            .iter()
            .map(|d| &d.code)
            .collect::<Vec<_>>()
    );
    if output.html.len() > MAX_HTML_BYTES
        || output
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error || d.code.starts_with("engine."))
    {
        stages["stopped_at"] = json!("render_rejected_full_source_fallback");
        return (stages, None);
    }
    stages["sanitizer"] = measure(|| sanitize(black_box(&output.html), &marker));
    let html = sanitize(&output.html, &marker);
    stages["sanitized_html_bytes"] = json!(html.len());
    if html.len() > MAX_HTML_BYTES || html.matches('<').count() > 8192 {
        stages["stopped_at"] = json!("sanitized_output_rejected_full_source_fallback");
        return (stages, None);
    }
    stages["stopped_at"] = json!("complete_sanitized_output");
    (stages, Some(html))
}

fn expected_fallback(source: &str) -> String {
    format!(
        "<pre class=\"markdown-fallback\">{}</pre>",
        source
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    )
}

async fn cache_costs(source: &str, expected: &super::RenderedBody) -> Value {
    let mut misses = Vec::new();
    let mut hits = Vec::new();
    let mut accounted_bytes = 0;
    for index in 0..WARMUPS + SAMPLES {
        // Fresh real cache setup is outside the timed miss; the call still uses
        // normal hashing, two permits, blocking work and cache insertion.
        let renderer = Renderer::default();
        let started = Instant::now();
        let first = renderer.render(black_box(source)).await;
        let miss = elapsed(started);
        let started = Instant::now();
        let second = renderer.render(black_box(source)).await;
        let hit = elapsed(started);
        assert_eq!(first.html, expected.html);
        assert_eq!(first.fallback, expected.fallback);
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(renderer.permits.available_permits(), 2);
        accounted_bytes = renderer.cached_bytes();
        assert!(accounted_bytes <= MAX_CACHE_BYTES);
        if index >= WARMUPS {
            misses.push(miss);
            hits.push(hit);
        }
    }
    json!({"miss": summary(misses), "hit": summary(hits), "accounted_bytes": accounted_bytes})
}

fn resource_source(index: usize) -> String {
    let prefix = format!("unique-body-{index:03} ");
    let suffix = format!(" END-{index:03}");
    format!(
        "{prefix}{}{suffix}",
        "a".repeat(MAX_BYTES - prefix.len() - suffix.len())
    )
}

async fn cache_resource_loop() -> Value {
    let renderer = Renderer::default();
    assert_eq!(renderer.permits.available_permits(), 2);
    let started = Instant::now();
    let mut records = Vec::new();
    let mut identities = HashSet::new();
    let mut max_observed_bytes = renderer.cached_bytes();
    for index in (0..RESOURCE_BODIES).step_by(2) {
        let a = resource_source(index);
        let b = resource_source(index + 1);
        let (first, second) = tokio::join!(renderer.render(&a), renderer.render(&b));
        for (source, output) in [(&a, first), (&b, second)] {
            assert_eq!(source.len(), MAX_BYTES);
            assert!(!output.fallback);
            assert!(
                output.html.contains(source.as_str()),
                "entire plain source must survive"
            );
            let source_digest = digest(source);
            assert!(identities.insert(source_digest.clone()));
            records.push(
                json!({"source_sha256": source_digest, "source_bytes": source.len(),
                "html_sha256": digest(&output.html), "html_bytes": output.html.len(),
                "fallback": output.fallback, "entire_source_preserved": true}),
            );
        }
        let bytes = renderer.cached_bytes();
        assert!(bytes <= MAX_CACHE_BYTES);
        max_observed_bytes = max_observed_bytes.max(bytes);
    }
    assert_eq!(identities.len(), RESOURCE_BODIES);
    assert_eq!(renderer.permits.available_permits(), 2);
    let cache = renderer.cache.lock().unwrap();
    assert!(cache.rows.len() <= super::MAX_CACHE_ITEMS);
    json!({"unique_bodies": RESOURCE_BODIES, "source_bytes_each": MAX_BYTES,
        "recipe":"unique ASCII prefix and suffix around one full plain paragraph",
        "elapsed_scope":"whole phase including source construction, rendering, integrity checks and digest records",
        "parse_status_evidence":"all actual product outputs passed its Complete parse gate and returned fallback=false",
        "concurrent_calls_per_batch": 2, "cpu_permits": 2, "elapsed_ns": elapsed(started),
        "final_accounted_cache_bytes": cache.bytes, "cached_items": cache.rows.len(),
        "max_observed_accounted_cache_bytes": max_observed_bytes,
        "cache_budget_bytes": MAX_CACHE_BYTES, "records": records})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "manual report-only Web pipeline/resource probe; run optimized with exact name"]
async fn report_current_product_pipeline_costs_and_cache_resources() {
    let process_id = std::process::id();
    println!(
        "{}",
        json!({"probe":"agentchat_web_mde_costs_v1", "phase":"start", "process_id":process_id})
    );
    std::io::stdout().flush().unwrap();
    let mut measurements = Vec::new();
    for recipe in recipes() {
        let (stages, staged_html) = stage_costs(&recipe.source);
        let cold = measure(|| render(black_box(&recipe.source)));
        let expected = render(&recipe.source);
        assert_eq!(expected.fallback, recipe.fallback, "{}", recipe.name);
        if expected.fallback {
            assert!(staged_html.is_none());
            assert_eq!(expected.html, expected_fallback(&recipe.source));
        } else {
            assert_eq!(staged_html.unwrap(), expected.html);
        }
        let cache = cache_costs(&recipe.source, &expected).await;
        measurements.push(
            json!({"recipe":recipe.name, "source_bytes":recipe.source.len(),
            "source_sha256":digest(&recipe.source), "stages":stages, "cold_product_render":cold,
            "fallback":expected.fallback, "html_bytes":expected.html.len(),
            "html_sha256":digest(&expected.html), "real_renderer_cache":cache}),
        );
    }
    // Release staged documents/outputs before the distinct cache workload.
    let cache_resources = cache_resource_loop().await;
    println!(
        "{}",
        json!({"probe":"agentchat_web_mde_costs_v1", "phase":"complete",
        "process_id":process_id, "target_os":std::env::consts::OS, "target_arch":std::env::consts::ARCH,
        "debug_assertions":cfg!(debug_assertions), "report_only":true,
        "warmups_per_operation":WARMUPS, "samples_per_operation":SAMPLES, "timing_unit":"nanoseconds",
        "timing_scope":"Calls through return; returned value teardown excluded. Stages follow actual fallback gates; skipped stages are null. Times are independent and are not an end-to-end sum or a latency certificate.",
        "resource_scope":"process RSS includes the test runtime, recipe validation and JSON/digest records; accounted cache bytes belong to the unchanged product cache",
        "profile":{"features":mentions::markdown_features(), "max_source_bytes":MAX_BYTES,
            "max_blocks":4096, "max_nesting_depth":64, "max_output_bytes":MAX_HTML_BYTES},
        "measurements":measurements, "cache_resources":cache_resources})
    );
}

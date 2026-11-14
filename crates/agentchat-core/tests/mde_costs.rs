//! Manual, report-only extraction costs through the actual public routing APIs.
use agentchat_core::mentions;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{hint::black_box, time::Instant};

const MAX_BYTES: usize = 65_536;
const WARMUPS: usize = 2;
const SAMPLES: usize = 5;

struct Recipe {
    name: &'static str,
    source: String,
    candidates: &'static [&'static str],
}

fn filled(bytes: usize, unit: &str, suffix: &str) -> String {
    let available = bytes.checked_sub(suffix.len()).unwrap();
    let mut body = unit.repeat(available / unit.len());
    body.push_str(&"x".repeat(available % unit.len()));
    body.push_str(suffix);
    assert_eq!(body.len(), bytes);
    body
}

fn image_alt(unit: &str) -> String {
    let prefix = "![";
    let suffix = "](image.png)";
    let alt = filled(MAX_BYTES - prefix.len() - suffix.len(), unit, "");
    let source = format!("{prefix}{alt}{suffix}");
    assert_eq!(source.len(), MAX_BYTES);
    source
}

fn recipes() -> Vec<Recipe> {
    vec![
        Recipe {
            name: "typical_short",
            source: "Please check @bob and @carol before release.".into(),
            candidates: &["bob", "carol"],
        },
        Recipe {
            name: "maximum_trailing_mention",
            source: filled(MAX_BYTES, "ordinary prose ", " @tail"),
            candidates: &["tail"],
        },
        Recipe {
            name: "maximum_multiline",
            source: filled(MAX_BYTES, "ordinary prose @bob\n", "\n\n@tail"),
            candidates: &["bob", "tail"],
        },
        Recipe {
            name: "maximum_many_blocks",
            source: filled(MAX_BYTES, "# heading\n\n", "\n\n@tail"),
            candidates: &["tail"],
        },
        Recipe {
            name: "plain_dense",
            source: filled(55_000, "@bob @carol ", ""),
            candidates: &["bob", "carol"],
        },
        Recipe {
            name: "formatted_dense",
            source: filled(55_000, "**@bo**b [@carol](https://example.test/path) ", ""),
            candidates: &["bob", "carol"],
        },
        Recipe {
            name: "maximum_image_alt_plain",
            source: image_alt("@bob @carol "),
            candidates: &["bob", "carol"],
        },
        Recipe {
            name: "maximum_image_alt_formatted",
            source: image_alt("**@bo**b &commat;carol "),
            candidates: &["bob", "carol"],
        },
    ]
}

fn measure<T>(mut operation: impl FnMut() -> T) -> Value {
    for _ in 0..WARMUPS {
        black_box(operation());
    }
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let started = Instant::now();
        let result = black_box(operation());
        samples.push(u64::try_from(started.elapsed().as_nanos()).unwrap());
        // Keep returned-value destruction outside the measured call. The real
        // tokens operation still destroys its internal document before return.
        black_box(result);
    }
    let mut sorted = samples.clone();
    sorted.sort_unstable();
    json!({"median_ns": sorted[SAMPLES / 2], "samples_ns": samples})
}

#[test]
#[ignore = "manual report-only routing-cost probe; run optimized with --ignored --nocapture"]
fn report_current_public_extraction_costs() {
    let mut measurements = Vec::new();
    for recipe in recipes() {
        let parse = measure(|| mentions::parse(black_box(&recipe.source)).unwrap());
        let parsed = mentions::parse(&recipe.source).unwrap();
        let candidates = mentions::delivery_tokens(&parsed).unwrap();
        assert_eq!(
            candidates.iter().map(String::as_str).collect::<Vec<_>>(),
            recipe.candidates,
            "{}",
            recipe.name
        );
        let scan = measure(|| mentions::delivery_tokens(black_box(&parsed)).unwrap());
        let parse_status = format!("{:?}", parsed.parse_status()).to_ascii_lowercase();
        // Do not retain the scan fixture's document during full-operation
        // measurements, which would manufacture a second live parse allocation.
        drop(parsed);
        assert_eq!(mentions::tokens(&recipe.source).unwrap(), candidates);
        let tokens = measure(|| mentions::tokens(black_box(&recipe.source)).unwrap());
        measurements.push(json!({
            "recipe": recipe.name,
            "source_bytes": recipe.source.len(),
            "source_sha256": Sha256::digest(recipe.source.as_bytes()).iter()
                .map(|byte| format!("{byte:02x}")).collect::<String>(),
            "parse_status": parse_status,
            "candidate_count": candidates.len(),
            "candidates": candidates,
            "parse": parse,
            "delivery_tokens_scan": scan,
            "tokens_operation": tokens,
        }));
    }
    let features = mentions::markdown_features();
    println!(
        "{}",
        json!({
            "probe": "agentchat_mde_routing_costs_v1",
            "report_only": true,
            "warmups_per_operation": WARMUPS,
            "samples_per_operation": SAMPLES,
            "timing_unit": "nanoseconds",
            "timing_scope": "Calls through return; returned value teardown excluded. tokens includes its internal ParsedDocument teardown. No recipient resolution, storage transaction or end-to-end latency is measured.",
            "process_id": std::process::id(),
            "target_os": std::env::consts::OS,
            "target_arch": std::env::consts::ARCH,
            "debug_assertions": cfg!(debug_assertions),
            "features": {
                "gfm": features.gfm,
                "syntax_highlight": features.syntax_highlight,
                "callouts": features.callouts,
                "raw_html": features.raw_html,
                "math": features.math,
                "mermaid": features.mermaid,
                "plantuml": features.plantuml,
            },
            "measurements": measurements,
        })
    );
}

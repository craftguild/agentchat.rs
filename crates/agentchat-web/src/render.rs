//! Bounded typed Markdown rendering and a byte-accounted immutable-body cache.

use oriha_core::{EngineLimits, OrihaEngine, ParseStatus, RenderInput, RenderMode, Severity};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex},
};
use tokio::sync::Semaphore;

#[cfg(test)]
#[path = "../tests/unit/mde_costs.rs"]
mod mde_costs;

/// Maximum normal rendered HTML before switching to complete escaped text.
pub const MAX_HTML_BYTES: usize = 256 * 1024;
/// Cache budget includes every key and retained rendered string.
pub const MAX_CACHE_BYTES: usize = 8 * 1024 * 1024;
const MAX_CACHE_ITEMS: usize = 256;

/// Safe computed output; the caller retains the unchanged original Markdown.
#[derive(Debug)]
pub struct RenderedBody {
    /// Sanitized semantic HTML, or escaped original text in a preformatted block.
    pub html: String,
    /// Rendering failed or exceeded a bound; complete original text is displayed.
    pub fallback: bool,
}

struct Cache {
    rows: VecDeque<([u8; 32], Arc<RenderedBody>)>,
    bytes: usize,
}
impl Default for Cache {
    fn default() -> Self {
        let rows = VecDeque::with_capacity(MAX_CACHE_ITEMS);
        let bytes = std::mem::size_of::<Self>()
            + rows.capacity() * std::mem::size_of::<([u8; 32], Arc<RenderedBody>)>();
        Self { rows, bytes }
    }
}
impl RenderedBody {
    fn retained_bytes(&self) -> usize {
        // Charge allocated string capacity and Arc's allocation/control block;
        // every deque slot/key is already reserved in Cache::default.
        self.html.capacity() + std::mem::size_of::<Self>() + 2 * std::mem::size_of::<usize>()
    }
}

/// Shared renderer. CPU permits live inside blocking jobs, so cancellation of a
/// request cannot release a permit while its CPU work is still running.
#[derive(Clone)]
pub struct Renderer {
    permits: Arc<Semaphore>,
    cache: Arc<Mutex<Cache>>,
}
impl Default for Renderer {
    fn default() -> Self {
        Self {
            permits: Arc::new(Semaphore::new(2)),
            cache: Arc::default(),
        }
    }
}
impl Renderer {
    pub(crate) fn plain_text(body: &str) -> Arc<RenderedBody> {
        Arc::new(escaped(body))
    }
    /// Render a bounded immutable message body outside the application coordinator.
    /// Identical bodies share computed output, independent of a restored message UUID.
    pub async fn render(&self, body: &str) -> Arc<RenderedBody> {
        let key: [u8; 32] = Sha256::digest(body.as_bytes()).into();
        if let Ok(mut cache) = self.cache.lock()
            && let Some(index) = cache.rows.iter().position(|(k, _)| *k == key)
        {
            let row = cache.rows.remove(index).expect("located cache entry");
            let result = row.1.clone();
            cache.rows.push_back(row);
            return result;
        }
        let Ok(permit) = self.permits.clone().acquire_owned().await else {
            return Arc::new(escaped(body));
        };
        let original = body.to_owned();
        let input = original.clone();
        let output = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            std::panic::catch_unwind(|| render(&input)).unwrap_or_else(|_| escaped(&input))
        })
        .await
        .unwrap_or_else(|_| escaped(&original));
        let output = Arc::new(output);
        if let Ok(mut cache) = self.cache.lock() {
            // Concurrent identical misses may finish together. Keep one accounted entry.
            if let Some(index) = cache.rows.iter().position(|(k, _)| *k == key) {
                let (_, old) = cache.rows.remove(index).expect("located cache entry");
                cache.bytes -= old.retained_bytes();
            }
            let bytes = output.retained_bytes();
            while !cache.rows.is_empty()
                && (cache.bytes + bytes > MAX_CACHE_BYTES || cache.rows.len() >= MAX_CACHE_ITEMS)
            {
                let (_, old) = cache.rows.pop_front().expect("nonempty cache");
                cache.bytes -= old.retained_bytes();
            }
            if bytes <= MAX_CACHE_BYTES {
                cache.bytes += bytes;
                cache.rows.push_back((key, output.clone()));
            }
        }
        output
    }

    /// Current accounted cache bytes, useful for resource acceptance checks.
    pub fn cached_bytes(&self) -> usize {
        self.cache
            .lock()
            .map(|cache| cache.bytes)
            .unwrap_or_default()
    }
}

fn render(body: &str) -> RenderedBody {
    let features = agentchat_core::mentions::markdown_features();
    let engine = OrihaEngine::default()
        .with_features(features.clone())
        .with_limits(EngineLimits {
            max_source_bytes: 65_536,
            max_blocks: 4096,
            max_nesting_depth: 64,
            max_output_bytes: MAX_HTML_BYTES,
        });
    // A source-authored safe HTML span must never impersonate a profile. This
    // private per-render marker is removed by the sanitizer and never retained
    // in output/cache. It authenticates only application-generated attributes.
    let marker = format!("{}:", uuid::Uuid::new_v4().simple());
    let parsed = engine.parse(RenderInput {
        markdown: body.to_owned(),
        mode: RenderMode::SemanticHtml,
        features,
        ..Default::default()
    });
    if parsed.parse_status() != ParseStatus::Complete {
        return escaped(body);
    }
    let Ok(decorated) = crate::render_mentions::decorate(&parsed, &marker) else {
        return escaped(body);
    };
    let output = engine.render_parsed(&decorated);
    if output.html.len() > MAX_HTML_BYTES
        || output.diagnostics.iter().any(|diagnostic| {
            diagnostic.severity == Severity::Error || diagnostic.code.starts_with("engine.")
        })
    {
        return escaped(body);
    }
    let html = sanitize(&output.html, &marker);
    if html.len() > MAX_HTML_BYTES || html.matches('<').count() > 8192 {
        return escaped(body);
    }
    RenderedBody {
        html,
        fallback: false,
    }
}

fn sanitize(html: &str, marker: &str) -> String {
    // No images or external resources. CSS classes support local syntax tokens;
    // only renderer-owned mention metadata survives; no style, event, SVG,
    // iframe or executable URL attribute survives. Raw Markdown HTML is disabled.
    let tags: HashSet<_> = [
        "p",
        "br",
        "hr",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "blockquote",
        "pre",
        "code",
        "span",
        "strong",
        "em",
        "del",
        "s",
        "a",
        "ul",
        "ol",
        "li",
        "table",
        "thead",
        "tbody",
        "tr",
        "th",
        "td",
        "input",
    ]
    .into_iter()
    .collect();
    let attributes = HashMap::from([
        ("a", HashSet::from(["href", "title"])),
        ("code", HashSet::from(["class"])),
        ("span", HashSet::from(["class", "data-mention"])),
        ("ol", HashSet::from(["start"])),
        ("input", HashSet::from(["type", "checked", "disabled"])),
    ]);
    let marker = marker.to_owned();
    ammonia::Builder::default()
        .tags(tags)
        .tag_attributes(attributes)
        .generic_attributes(HashSet::new())
        .attribute_filter(move |element, attribute, value| {
            if element == "span" && attribute == "data-mention" {
                value
                    .strip_prefix(marker.as_str())
                    .map(|handle| std::borrow::Cow::Owned(handle.to_owned()))
            } else {
                Some(std::borrow::Cow::Borrowed(value))
            }
        })
        .url_schemes(HashSet::from(["http", "https", "mailto"]))
        .link_rel(Some("noopener noreferrer nofollow"))
        .clean(html)
        .to_string()
}

fn escaped(body: &str) -> RenderedBody {
    let mut html = String::from("<pre class=\"markdown-fallback\">");
    for character in body.chars() {
        match character {
            '&' => html.push_str("&amp;"),
            '<' => html.push_str("&lt;"),
            '>' => html.push_str("&gt;"),
            _ => html.push(character),
        }
    }
    html.push_str("</pre>");
    RenderedBody {
        html,
        fallback: true,
    }
}

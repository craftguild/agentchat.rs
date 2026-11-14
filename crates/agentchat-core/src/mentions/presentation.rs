//! One source-aware Markdown interpretation for routing and display candidates.
use crate::{Error, ErrorCode, Result};
use oriha_core::{
    EngineLimits, FeatureFlags, OrihaEngine, ParseStatus, ParsedDocument, RegionId, RenderInput,
    RenderMode,
};
use std::ops::Range;

/// A normalized token and byte range in `MentionText::text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionOccurrence {
    /// Lowercase individual or reserved handle, without the at-sign.
    pub handle: String,
    /// UTF-8 range including the at-sign.
    pub range: Range<usize>,
}
/// A candidate tied to exactly one parsed document's visible region.
#[derive(Debug, Clone)]
pub struct RegionMention {
    /// Lowercase individual or reserved handle.
    pub handle: String,
    /// Identity belonging to the supplied parsed document only.
    pub region: RegionId,
    /// UTF-8 range within that region.
    pub range: Range<usize>,
}
/// Compatibility projection; delivery and annotation use typed region identities.
pub struct MentionText {
    /// Eligible regions separated by spaces, without reparsing Markdown.
    pub text: String,
    /// Repeated occurrences in document order.
    pub occurrences: Vec<MentionOccurrence>,
}

/// Explicit syntax profile shared with the read-only Viewer.
pub fn markdown_features() -> FeatureFlags {
    FeatureFlags {
        gfm: true,
        syntax_highlight: true,
        callouts: false,
        raw_html: false,
        math: false,
        mermaid: false,
        plantuml: false,
    }
}

/// Parse a routing body once, refusing known omitted or failed parsing.
///
/// The source-byte bound limits direct block count. Depth is deliberately bounded;
/// reaching it is an explicit mutation error, never a successful empty recipient set.
pub fn parse(markdown: &str) -> Result<ParsedDocument> {
    let parsed = OrihaEngine::default()
        .with_features(markdown_features())
        .with_limits(EngineLimits {
            max_source_bytes: 65_536,
            max_blocks: 65_536,
            max_nesting_depth: 64,
            max_output_bytes: 262_144,
        })
        .parse(RenderInput {
            markdown: markdown.into(),
            mode: RenderMode::SemanticHtml,
            ..Default::default()
        });
    complete(&parsed)?;
    Ok(parsed)
}

fn complete(parsed: &ParsedDocument) -> Result<()> {
    if parsed.parse_status() == ParseStatus::Complete {
        return Ok(());
    }
    let status = match parsed.parse_status() {
        ParseStatus::Complete => unreachable!(),
        ParseStatus::Incomplete => "incomplete",
        ParseStatus::Failed => "failed",
    };
    let mut error = Error::new(
        ErrorCode::InvalidArgument,
        "Markdown extraction is unavailable; no message was committed.",
    );
    error.details = serde_json::json!({"markdown_parse_status":status,
        "diagnostic_codes":parsed.diagnostics().iter().take(16).map(|d|d.code.as_str()).collect::<Vec<_>>()});
    Err(error)
}

/// Extract visible, source-authored candidates without resolving recipients.
///
/// Selected display scope: rendered footnotes participate and images/emoji end
/// regions. Delivery additionally includes authored image alt via `delivery_tokens`.
pub fn occurrences(parsed: &ParsedDocument) -> Result<Vec<RegionMention>> {
    complete(parsed)?;
    let mut result = Vec::new();
    for region in parsed.text_regions() {
        for item in
            super::lexer::scan_mapped(&region.text, region.mappings.iter().map(|m| &m.mapping))
        {
            result.push(RegionMention {
                handle: item.handle,
                region: region.id,
                range: item.range,
            });
        }
    }
    Ok(result)
}

/// Project candidates to a plain text coordinate space for existing Rust callers.
pub fn presentation(markdown: &str) -> Result<MentionText> {
    let parsed = parse(markdown)?;
    let mut found = occurrences(&parsed)?.into_iter().peekable();
    let mut text = String::new();
    let mut items = Vec::new();
    for region in parsed.text_regions() {
        if !text.is_empty() {
            text.push(' ');
        }
        let offset = text.len();
        text.push_str(&region.text);
        while found.peek().is_some_and(|item| item.region == region.id) {
            let item = found.next().expect("peeked occurrence");
            items.push(MentionOccurrence {
                handle: item.handle,
                range: offset + item.range.start..offset + item.range.end,
            });
        }
    }
    Ok(MentionText {
        text,
        occurrences: items,
    })
}

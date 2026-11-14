//! Typed immutable mention annotations; source HTML never supplies authority.
use agentchat_core::{Result, mentions};
use oriha_core::{AnnotationAttributes, AnnotationMetadata, ParsedDocument, TextAnnotation};
use std::{borrow::Cow, collections::BTreeMap};

// In the pinned renderer every distinct annotation emits at least this wrapper
// before sanitization. Split formatting can add wrappers, never remove them.
const MIN_ANNOTATION_BYTES: usize =
    r#"<span data-mde-annotation-kind="" data-mde-annotation-id=""></span>"#.len();

pub(crate) fn decorate<'a>(
    parsed: &'a ParsedDocument,
    marker: &str,
) -> Result<Cow<'a, ParsedDocument>> {
    let occurrences = mentions::occurrences(parsed)?;
    if occurrences.is_empty() {
        return Ok(Cow::Borrowed(parsed));
    }
    // Avoid allocating an annotation plan already guaranteed to exceed the
    // existing raw-HTML budget. The caller preserves the complete original.
    if occurrences.len() > crate::render::MAX_HTML_BYTES / MIN_ANNOTATION_BYTES {
        return Err(agentchat_core::Error::new(
            agentchat_core::ErrorCode::InvalidArgument,
            "Mention annotation output exceeds the render budget.",
        ));
    }
    let annotations = occurrences
        .into_iter()
        .enumerate()
        .map(|(i, item)| TextAnnotation {
            region: item.region,
            range: item.range,
            metadata: AnnotationMetadata {
                kind: "mention".into(),
                id: format!("mention-{i}"),
                attributes: AnnotationAttributes {
                    classes: vec!["mention".into()],
                    data: BTreeMap::from([("mention".into(), format!("{marker}{}", item.handle))]),
                },
            },
        })
        .collect::<Vec<_>>();
    parsed
        .apply_annotations(&annotations)
        .map(Cow::Owned)
        .map_err(|_| {
            agentchat_core::Error::new(
                agentchat_core::ErrorCode::IntegrityError,
                "Mention annotations failed validation.",
            )
        })
}

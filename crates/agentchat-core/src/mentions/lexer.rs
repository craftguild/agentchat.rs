//! Shared source-aware lexical checks for visible text and image alternatives.
use super::MentionOccurrence;
use oriha_core::{TextMapping, TextProvenance};

pub(super) fn scan_mapped<'a>(
    text: &str,
    mappings: impl Iterator<Item = &'a TextMapping> + Clone,
) -> Vec<MentionOccurrence> {
    let mut found = Vec::new();
    let mut cursor = mappings.peekable();
    for (start, c) in text.char_indices() {
        if c != '@' {
            continue;
        }
        if let Some(previous) = text[..start].chars().next_back()
            && (previous.is_alphanumeric() || "_-/.:@%".contains(previous))
        {
            continue;
        }
        let tail = &text[start + 1..];
        if !tail.starts_with(|c: char| c.is_ascii_alphabetic()) {
            continue;
        }
        let len = tail
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'-')
            .count();
        let end = start + 1 + len;
        while cursor.peek().is_some_and(|m| m.display.end <= start) {
            cursor.next();
        }
        if !cursor.peek().is_some_and(|m| {
            m.display.contains(&start)
                && !m.source.is_empty()
                && matches!(
                    m.provenance,
                    TextProvenance::Literal | TextProvenance::CharacterReference
                )
        }) {
            continue;
        }
        // Walk only this token's mappings, retaining the advancing outer cursor.
        // An original at-sign must not authorize a generated handle suffix.
        let mut covered = start;
        for mapping in cursor.clone() {
            if mapping.display.start > covered
                || mapping.source.is_empty()
                || mapping.provenance == TextProvenance::Generated
            {
                break;
            }
            covered = mapping.display.end.min(end);
            if covered == end {
                found.push(MentionOccurrence {
                    handle: tail[..len].to_ascii_lowercase(),
                    range: start..end,
                });
                break;
            }
        }
    }
    found
}

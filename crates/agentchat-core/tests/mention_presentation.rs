//! Presentation offsets must preserve lexical policy without delivery effects.
use agentchat_core::mentions::{presentation, tokens};

#[test]
fn repeated_escaped_formatted_and_group_mentions_keep_exact_visible_ranges() {
    let body = r"日本語 @Alice \@Alice @ali**ce** @all @here @channel &#64;Bob";
    let parsed = presentation(body).unwrap();
    assert_eq!(
        parsed
            .occurrences
            .iter()
            .map(|item| item.handle.as_str())
            .collect::<Vec<_>>(),
        ["alice", "alice", "all", "here", "channel", "bob"]
    );
    for item in parsed.occurrences {
        assert_eq!(
            parsed.text[item.range].to_ascii_lowercase(),
            format!("@{}", item.handle)
        );
    }
    assert!(tokens(body).unwrap().contains("alice"));
}

#[test]
fn code_emails_paths_attributes_and_escaped_duplicates_never_become_profiles() {
    let body = "`@code`\n\n```text\n@fenced\n```\n\na@b.test https://example.test/@path <i title='@attribute'>@html</i> \\@only @valid \\@valid";
    let parsed = presentation(body).unwrap();
    assert_eq!(
        parsed
            .occurrences
            .iter()
            .map(|item| item.handle.as_str())
            .collect::<Vec<_>>(),
        ["html", "valid"]
    );
}

#[test]
fn wikilink_syntax_stays_literal_without_duplicating_trailing_mentions() {
    // Product presentation uses the default parser, with wikilinks disabled.
    let prefix = "[[a|]] ".repeat(16);
    let body = format!("{prefix}@Known");
    let parsed = presentation(&body).unwrap();
    assert_eq!(parsed.text.trim_end(), body);
    assert_eq!(parsed.occurrences.len(), 1);
    let occurrence = &parsed.occurrences[0];
    assert_eq!(occurrence.handle, "known");
    assert_eq!(&parsed.text[occurrence.range.clone()], "@Known");
    assert_eq!(occurrence.range.start, prefix.len());
}

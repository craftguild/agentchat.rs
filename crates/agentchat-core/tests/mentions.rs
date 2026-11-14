//! T07: visible Markdown, complete tokens and frozen participant snapshots.

use agentchat_core::mentions::{self, MentionParticipant, MentionSource, MentionState};
use uuid::Uuid;

#[test]
fn markdown_and_boundaries() {
    let cases = [
        ("@Bob, (@ann)! @BOB", vec!["ann", "bob"]),
        (r"\@bob @ann", vec!["ann"]),
        (r"\\@bob", vec!["bob"]),
        ("`@bob` @ann", vec!["ann"]),
        ("```rs\n@bob\n```\n@ann", vec!["ann"]),
        ("    @bob\n\n@ann", vec!["ann"]),
        (
            "[@bob](https://x/@ignored) ![x](https://x/@ignored)",
            vec!["bob"],
        ),
        (
            "mail@bob.com https://host/@bob word@bob é@bob @bob-long.",
            vec!["bob-long"],
        ),
        ("**@bob** @**ann**", vec!["ann", "bob"]),
        ("word**x@bob**", vec![]),
        ("<!-- @bob -->\n\n@ann", vec!["ann"]),
        ("&#64;bob", vec!["bob"]),
    ];
    for (text, expected) in cases {
        assert_eq!(
            mentions::tokens(text)
                .unwrap()
                .into_iter()
                .collect::<Vec<_>>(),
            expected,
            "{text}"
        );
    }
}

#[test]
fn recipients_are_joined_deduplicated_and_exclude_sender() {
    let sender = Uuid::new_v4();
    let online = Uuid::new_v4();
    let offline = Uuid::new_v4();
    let participants = vec![
        MentionParticipant {
            id: sender,
            handle: "sender".into(),
            joined: true,
            in_channel: true,
            connected: true,
            wake_deferred: false,
        },
        MentionParticipant {
            id: online,
            handle: "online".into(),
            joined: true,
            in_channel: true,
            connected: true,
            wake_deferred: false,
        },
        MentionParticipant {
            id: offline,
            handle: "offline".into(),
            joined: true,
            in_channel: true,
            connected: false,
            wake_deferred: false,
        },
        MentionParticipant {
            id: Uuid::new_v4(),
            handle: "left".into(),
            joined: false,
            in_channel: false,
            connected: true,
            wake_deferred: false,
        },
    ];
    assert!(
        mentions::resolve("passive", sender, &participants)
            .unwrap()
            .recipients
            .is_empty()
    );
    let here = mentions::resolve("@here", sender, &participants)
        .unwrap()
        .recipients;
    assert_eq!(here.len(), 1);
    assert!(here[&online].contains(&MentionSource::Here));
    let all = mentions::resolve(
        "@all @channel @offline @OFFLINE @sender",
        sender,
        &participants,
    )
    .unwrap()
    .recipients;
    assert_eq!(all.len(), 2);
    assert_eq!(all[&offline].len(), 3);
    let unresolved = mentions::resolve(
        "@unknown @left @offline @sender @online",
        sender,
        &participants,
    )
    .unwrap();
    assert_eq!(unresolved.recipients.len(), 2);
    assert_eq!(
        unresolved
            .individuals
            .iter()
            .map(|i| i.state)
            .collect::<Vec<_>>(),
        vec![
            MentionState::Left,
            MentionState::Offline,
            MentionState::Queued,
            MentionState::SelfMention,
            MentionState::Unknown
        ]
    );
    assert_eq!(
        unresolved
            .warnings
            .iter()
            .map(|w| w.code.as_str())
            .collect::<Vec<_>>(),
        vec!["recipient_left", "recipient_offline", "unknown_recipient"]
    );
}

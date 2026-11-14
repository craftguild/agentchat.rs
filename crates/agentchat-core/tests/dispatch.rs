//! Sending identity/subtype policy is independent of audit event recording.

use agentchat_core::{
    model::{Dispatch, DispatchKind, DispatchPayload, Message},
    read::Query,
    sequence::Sequence,
    time::Timestamp,
};
use uuid::Uuid;

fn message() -> (Dispatch, Message) {
    let header = Dispatch {
        id: Uuid::new_v4(),
        kind: DispatchKind::Message,
        channel_id: Some(Uuid::new_v4()),
        created_at: Timestamp::parse("2026-11-01T00:00:00.000Z").unwrap(),
        creation_seq: Sequence::new(2).unwrap(),
    };
    let message = Message {
        id: header.id,
        channel_id: header.channel_id.unwrap(),
        author_id: Uuid::new_v4(),
        root_message_id: None,
        body: "Original 日本語\0 bytes".into(),
        created_at: header.created_at,
        creation_seq: header.creation_seq,
    };
    (header, message)
}

#[test]
fn dispatch_kind_accepts_exactly_message_or_notice() {
    for (value, kind) in [
        ("message", DispatchKind::Message),
        ("notice", DispatchKind::Notice),
    ] {
        assert_eq!(value.parse::<DispatchKind>().unwrap(), kind);
        assert_eq!(kind.as_str(), value);
        assert_eq!(serde_json::to_value(kind).unwrap(), value);
        assert_eq!(
            serde_json::from_value::<DispatchKind>(value.into()).unwrap(),
            kind
        );
    }
    for value in [
        "",
        "Message",
        "event",
        "system",
        "message.created",
        "notice ",
    ] {
        assert!(value.parse::<DispatchKind>().is_err());
        assert!(serde_json::from_value::<DispatchKind>(value.into()).is_err());
    }
}

#[test]
fn public_message_is_the_exact_projection_of_its_header() {
    let (header, content) = message();
    header
        .validate_payload(&DispatchPayload::Message(content.clone()))
        .unwrap();
    let json = serde_json::to_value(&content).unwrap();
    assert_eq!(json["id"], header.id.to_string());
    assert_eq!(json["body"], "Original 日本語\0 bytes");
    assert!(json.get("dispatch_id").is_none());
    for mutation in 0..7 {
        let mut changed = content.clone();
        match mutation {
            0 => changed.id = Uuid::new_v4(),
            1 => changed.channel_id = Uuid::new_v4(),
            2 => changed.created_at = changed.created_at.after_seconds(1).unwrap(),
            3 => changed.creation_seq = Sequence::new(3).unwrap(),
            4 => changed.author_id = Uuid::nil(),
            5 => changed.root_message_id = Some(header.id),
            _ => changed.body = "\u{2003}\n".into(),
        }
        assert!(
            header
                .validate_payload(&DispatchPayload::Message(changed))
                .is_err(),
            "mutation {mutation}"
        );
    }
}

#[test]
fn notice_header_cannot_acquire_message_content_or_become_public_message_query() {
    let (mut header, content) = message();
    header.kind = DispatchKind::Notice;
    assert!(
        header
            .validate_payload(&DispatchPayload::Message(content))
            .is_err()
    );
    let id = Uuid::new_v4();
    let query: Query = serde_json::from_value(serde_json::json!({
        "operation":"message", "message_id":id
    }))
    .unwrap();
    assert!(matches!(query, Query::Message { message_id, .. } if message_id == id));
    assert!(
        serde_json::from_value::<Query>(serde_json::json!({
            "operation":"dispatch", "dispatch_id":id
        }))
        .is_err()
    );
}

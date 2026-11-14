//! Typed Notice framing cannot carry an audit envelope or Message body.
use agentchat_core::{
    delivery::{MAX_WAKE_BYTES, MessageInput, NativeInput, NoticeInput},
    model::{Dispatch, DispatchKind, DispatchNotice, NoticeKind},
    project::InputReceiptPolicy,
    sequence::Sequence,
    time::Timestamp,
};
use serde_json::{Value, json};
use uuid::Uuid;
fn notice(pattern: String) -> NoticeInput {
    let id = Uuid::from_u128;
    let now = Timestamp::parse("2026-10-31T00:00:00.000Z").unwrap();
    NoticeInput::new(id(1),id(2),id(3),&Dispatch{id:id(4),kind:DispatchKind::Notice,channel_id:None,created_at:now,creation_seq:Sequence::new(9_007_199_254_740_993).unwrap()},&DispatchNotice{dispatch_id:id(4),kind:NoticeKind::ReservationAcquired,data:json!({"reservation_id":id(5),"owner":{"actor_id":id(6),"kind":"agent","handle":"owner"},"generation":1,"state":"active","ttl_seconds":60,"acquired_at":now,"expires_at":now.after_seconds(60).unwrap(),"released_at":null,"reason":null,"patterns":[pattern]})},true,InputReceiptPolicy{auto_read:true}).unwrap()
}
#[test]
fn canonical_notice_is_distinct_without_an_audit_event_or_message_author() {
    let input = notice("crates/**".into());
    let text = input.text().unwrap();
    assert_eq!(
        text,
        format!(
            "[AgentChat:Notice]\n--\n{}\n--\n[/AgentChat:Notice]",
            serde_json::to_string(&input).unwrap()
        )
    );
    assert_eq!(NoticeInput::from_text(&text).unwrap(), input);
    assert_eq!(
        NativeInput::from_text(&text).unwrap(),
        NativeInput::Notice(input)
    );
    assert!(MessageInput::from_text(&text).is_err());
    let value: Value = serde_json::from_str(text.lines().nth(2).unwrap()).unwrap();
    assert_eq!(value["dispatch"]["creation_seq"], "9007199254740993");
    assert_eq!(value["frame_revision"], "dispatch_v4");
    for key in ["event", "body", "body_utf8_bytes", "author"] {
        assert!(value.get(key).is_none());
    }
}
#[test]
fn oversized_notice_data_is_explicitly_omitted_without_changing_header() {
    let input = notice("x".repeat(MAX_WAKE_BYTES));
    assert!(input.data_omitted);
    assert!(input.notice.data.is_null());
    assert!(input.text().unwrap().len() <= MAX_WAKE_BYTES);
    assert_eq!(
        NoticeInput::from_text(&input.text().unwrap()).unwrap(),
        input
    );
    let mut scoped = input.clone();
    scoped.dispatch.channel_id = Some(Uuid::from_u128(55));
    assert!(scoped.text().is_err());
    let mut bad = input.clone();
    bad.dispatch.kind = DispatchKind::Message;
    assert!(bad.text().is_err());
    bad = input.clone();
    bad.notice.dispatch_id = Uuid::from_u128(88);
    assert!(bad.text().is_err());
    bad = input;
    bad.notice.data = json!({});
    assert!(bad.text().is_err());
}
#[test]
fn malformed_and_old_event_or_revision_frames_are_refused() {
    let text = notice("crates/**".into()).text().unwrap();
    for changed in [
        format!("{text}\n"),
        text.replacen('{', "{ ", 1),
        text.replacen('{', "{\"event\":{},", 1),
        text.replace("AgentChat:Notice", "AgentChat:Event"),
        text.replace("dispatch_v4", "read_policy_v3"),
        text.replace("\"frame_revision\":\"dispatch_v4\",", ""),
        text.replace("\n--\n", "\r\n--\r\n"),
    ] {
        assert!(NoticeInput::from_text(&changed).is_err());
        assert!(NativeInput::from_text(&changed).is_err());
    }
}
#[test]
fn frozen_read_policy_is_required_and_ack_metadata_is_rejected() {
    for auto_read in [false, true] {
        let mut input = notice("crates/**".into());
        input.auto_read = auto_read;
        let parsed = NativeInput::from_text(&input.text().unwrap()).unwrap();
        assert_eq!(parsed.receipt_policy(), InputReceiptPolicy { auto_read });
    }
    let text = notice("crates/**".into()).text().unwrap();
    assert!(NoticeInput::from_text(&text.replace("\"auto_read\":true,", "")).is_err());
    assert!(NoticeInput::from_text(&text.replacen('{', "{\"auto_ack\":true,", 1)).is_err());
}

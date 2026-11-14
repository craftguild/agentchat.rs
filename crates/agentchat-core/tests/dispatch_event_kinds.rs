//! Fresh typed occurrence and notification boundaries, without audit-driven sends.
use agentchat_core::{
    model::{Event, EventKind, NoticeKind},
    sequence::Sequence,
    time::Timestamp,
};
use serde_json::{Value, json};
use uuid::Uuid;
fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}
fn time() -> Timestamp {
    Timestamp::parse("2026-10-31T00:00:00.000Z").unwrap()
}
/// Kind, payload and expected actor/channel/Message envelope attribution.
type OccurrenceCase = (EventKind, Value, Option<Uuid>, Option<Uuid>, Option<Uuid>);
fn cases() -> Vec<OccurrenceCase> {
    use EventKind::*;
    let owner = json!({"actor_id":id(1),"kind":"agent","handle":"owner"});
    let lease = |state: &str, released: Value| json!({"reservation_id":id(8),"owner":owner,"generation":5,"state":state,"ttl_seconds":60,"acquired_at":time(),"expires_at":time().after_seconds(60).unwrap(),"released_at":released,"reason":null,"patterns":["crates/**"]});
    vec![
        (
            AgentJoined,
            json!({"agent_id":id(1),"handle":"owner"}),
            Some(id(1)),
            None,
            None,
        ),
        (
            AgentLeft,
            json!({"agent_id":id(1),"handle":"owner"}),
            Some(id(1)),
            None,
            None,
        ),
        (
            AgentLaunchRetired,
            json!({"agent_id":id(1),"handle":"owner"}),
            Some(id(1)),
            None,
            None,
        ),
        (
            UserJoined,
            json!({"actor_id":id(1),"kind":"user","handle":"user"}),
            Some(id(1)),
            None,
            None,
        ),
        (
            UserLeft,
            json!({"actor_id":id(1),"kind":"user","handle":"user"}),
            Some(id(1)),
            None,
            None,
        ),
        (
            ChannelCreated,
            json!({"name":"dev"}),
            Some(id(1)),
            Some(id(2)),
            None,
        ),
        (
            ChannelJoined,
            json!({"agent_id":id(1),"channel_id":id(2)}),
            Some(id(1)),
            Some(id(2)),
            None,
        ),
        (
            ChannelLeft,
            json!({"agent_id":id(1),"channel_id":id(2)}),
            Some(id(1)),
            Some(id(2)),
            None,
        ),
        (
            MessageCreated,
            json!({"thread_id":id(4),"mentioned_agent_ids":[id(5)],"preview":"hello","body_bytes":5,"mention_results":[],"warnings":[]}),
            Some(id(1)),
            Some(id(2)),
            Some(id(4)),
        ),
        (
            MessageDeliveryConfirmationTimeout,
            json!({"message_id":id(4),"thread_id":id(4),"channel_id":id(2),"author_id":id(1),"observed_at":time().after_seconds(61).unwrap(),"expired_count":1,"sample_recipients":[{"recipient":{"actor_id":id(5),"kind":"agent","handle":"reader"},"delivery_id":id(6),"state":"failed_timeout","deadline_at":time().after_seconds(60).unwrap(),"timed_out_at":time().after_seconds(61).unwrap()}],"preview":"hello"}),
            None,
            Some(id(2)),
            Some(id(4)),
        ),
        (
            ReservationAcquired,
            lease("active", Value::Null),
            Some(id(1)),
            None,
            None,
        ),
        (
            ReservationReleased,
            lease("released", json!(time().after_seconds(1).unwrap())),
            Some(id(1)),
            None,
            None,
        ),
        (
            ReservationExpired,
            lease("expired", Value::Null),
            None,
            None,
            None,
        ),
        (
            ProjectChannelPolicyMaterialized,
            json!({"default_channels":["dev"],"primary_channel":"dev","removed_legacy_memberships":0}),
            None,
            None,
            None,
        ),
        (
            ProjectChannelPolicyChanged,
            json!({"default_channels":["dev"],"primary_channel":"dev"}),
            None,
            None,
            None,
        ),
    ]
}
#[test]
fn all_produced_kinds_have_strict_payload_and_envelope_validation() {
    let cases = cases();
    assert_eq!(cases.len(), 15);
    for (kind, data, actor_id, channel_id, message_id) in cases {
        let event = Event {
            version: 1,
            event_id: id(99),
            seq: Sequence::new(9).unwrap(),
            kind,
            occurred_at: time(),
            actor_id,
            channel_id,
            message_id,
            data,
        };
        event.validate().unwrap();
        assert_eq!(event.kind.as_str().parse::<EventKind>().unwrap(), kind);
        assert_eq!(
            serde_json::from_value::<EventKind>(json!(kind.as_str())).unwrap(),
            kind
        );
        let mut wrong = event.clone();
        wrong.data["unknown"] = json!(true);
        assert!(wrong.validate().is_err(), "{kind}");
        wrong = event.clone();
        wrong.actor_id = Some(id(100));
        assert!(
            wrong.validate().is_err()
                || kind == EventKind::MessageCreated
                || kind == EventKind::ChannelCreated,
            "{kind}"
        );
        if let Ok(notice) = NoticeKind::try_from(kind) {
            notice.validate_dispatch(channel_id, &event.data).unwrap();
            assert_eq!(notice.as_str(), kind.as_str());
            let other = if channel_id.is_some() {
                None
            } else {
                Some(id(2))
            };
            assert!(notice.validate_dispatch(other, &event.data).is_err());
        } else {
            assert_eq!(kind, EventKind::MessageCreated);
        }
    }
}
#[test]
fn unknown_legacy_and_message_as_notice_kinds_are_refused() {
    for kind in [
        "message.read_timeout",
        "agent.unknown",
        "MessageCreated",
        "",
        "message.created ",
    ] {
        assert!(kind.parse::<EventKind>().is_err());
        assert!(kind.parse::<NoticeKind>().is_err());
    }
    assert!("message.created".parse::<NoticeKind>().is_err());
    assert!(serde_json::from_value::<NoticeKind>(json!("message.created")).is_err());
    assert!(
        EventKind::AgentJoined
            .validate_data(&json!({"actor_id":id(1),"kind":"user","handle":"user"}))
            .is_err()
    );
}
#[test]
fn timeout_identity_channel_and_strict_deadline_are_not_interchangeable() {
    let (_, data, _, channel, _) = cases()
        .into_iter()
        .find(|r| r.0 == EventKind::MessageDeliveryConfirmationTimeout)
        .unwrap();
    let kind = NoticeKind::MessageDeliveryConfirmationTimeout;
    assert!(kind.validate_dispatch(Some(id(33)), &data).is_err());
    let mut wrong = data.clone();
    wrong["sample_recipients"][0]["timed_out_at"] =
        wrong["sample_recipients"][0]["deadline_at"].clone();
    assert!(kind.validate_dispatch(channel, &wrong).is_err());
    wrong = data;
    wrong["sample_recipients"][0]["recipient"]["kind"] = json!("user");
    assert!(kind.validate_dispatch(channel, &wrong).is_err());
}

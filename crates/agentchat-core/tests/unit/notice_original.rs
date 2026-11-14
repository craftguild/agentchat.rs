//! Exact inclusive current-frame budget; old frames have no recovery decoder.
use super::*;
use serde_json::json;
#[test]
fn current_notice_budget_is_inclusive_and_old_policy_headers_are_refused() {
    let id = Uuid::from_u128;
    let now = time::Timestamp::parse("2026-10-31T00:00:00.000Z").unwrap();
    let header = Dispatch {
        id: id(4),
        kind: DispatchKind::Notice,
        channel_id: None,
        created_at: now,
        creation_seq: sequence::Sequence::new(9).unwrap(),
    };
    let content = DispatchNotice {
        dispatch_id: id(4),
        kind: NoticeKind::ReservationAcquired,
        data: json!({"reservation_id":id(5),"owner":{"actor_id":id(6),"kind":"agent","handle":"owner"},"generation":1,"state":"active","ttl_seconds":60,"acquired_at":now,"expires_at":now.after_seconds(60).unwrap(),"released_at":null,"reason":null,"patterns":[""]}),
    };
    let mut content = content;
    content.data["patterns"][0] = json!("x");
    let policy = InputReceiptPolicy { auto_read: false };
    let baseline = NoticeInput::new(id(1), id(2), id(3), &header, &content, false, policy)
        .unwrap()
        .text()
        .unwrap()
        .len()
        - 1;
    for extra in [-1, 0, 1] {
        let size = (MAX_WAKE_BYTES as isize + extra) as usize;
        content.data["patterns"][0] = json!("x".repeat(size - baseline));
        let input =
            NoticeInput::new(id(1), id(2), id(3), &header, &content, false, policy).unwrap();
        assert_eq!(input.data_omitted, extra > 0);
        assert!(!input.auto_read);
        let text = input.text().unwrap();
        if extra <= 0 {
            assert_eq!(text.len(), size);
            assert_eq!(input.notice.data, content.data);
        } else {
            assert!(input.notice.data.is_null());
            assert!(text.len() < MAX_WAKE_BYTES);
        }
        assert_eq!(NoticeInput::from_text(&text).unwrap(), input);
        assert!(NoticeInput::from_text(&text.replace(",\"auto_read\":false", "")).is_err());
    }
}

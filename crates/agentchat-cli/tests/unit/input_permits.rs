//! Continuations bind accepted whole bytes independently of the AGENTCHAT FIFO head.
use super::*;
use serde_json::json;

fn permit(body: &str, token: u128) -> (Permit, String) {
    let token = Uuid::from_u128(token);
    let root = Uuid::from_u128(1);
    let context = Uuid::from_u128(2);
    let text =
        format!("AGENTCHAT_INPUT_NOTICE root={root} context={context} token={token}\n{body}");
    let digest: [u8; 32] = Sha256::digest(text.as_bytes()).into();
    let issued: u64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap();
    let p = serde_json::from_value(json!({"root":root,"generation":1,"context_id":context,
        "incarnation":Uuid::from_u128(3),"token":token,"epoch":4,"issued":issued,
        "queue":{"id":Uuid::new_v4(),"digest":digest,"bytes":text.len()}}))
    .unwrap();
    (p, text)
}
#[test]
fn whole_maximum_raw_bytes_and_multiple_queued_continuations_are_supported() {
    let (first, a) = permit(&format!("{}\r\n末尾 \t\n", "\0".repeat(65_500)), 10);
    let (second, b) = permit(
        "second\nAGENTCHAT_INPUT_NOTICE root=inside original body\n",
        11,
    );
    let input = json!({"session_id":first.root});
    first.valid(&input, true).unwrap();
    second.valid(&input, true).unwrap();
    frames(
        &format!("<task-notification>\n{a}\n{b}\n</task-notification>"),
        &[first, second],
    )
    .unwrap();
}
#[test]
fn partial_changed_replayed_and_unproved_extra_frames_are_rejected() {
    for mode in [
        "partial",
        "changed",
        "replayed",
        "extra",
        "duplicate_id",
        "overlap",
    ] {
        let (p, a) = permit("exact body\n末尾", 10);
        let (mut q, b) = permit("another body", 11);
        let mut text = format!("<task-notification>{a}{b}</task-notification>");
        match mode {
            "partial" => text = text.replace("exact body", "exact"),
            "changed" => text = text.replace("末尾", "変更"),
            "replayed" => text.push_str(&a),
            "extra" => text.push_str("AGENTCHAT_INPUT_NOTICE root=unproved"),
            "duplicate_id" => q.queue.as_mut().unwrap().id = p.queue.as_ref().unwrap().id,
            "overlap" => {
                q = permit("exact body\n末尾", 10).0;
            }
            _ => unreachable!(),
        }
        assert!(frames(&text, &[p, q]).is_err(), "{mode}");
    }
}
#[test]
fn notice_and_final_permits_have_separate_age_and_queue_requirements() {
    let (mut p, _) = permit("original", 10);
    let input = json!({"session_id":p.root});
    assert!(p.valid(&input, false).is_err());
    p.issued = Some(0);
    assert!(p.valid(&input, true).is_err());
    p.issued = Some(u64::MAX);
    assert!(p.valid(&input, true).is_err());
    p.issued = None;
    p.queue = None;
    p.valid(&input, false).unwrap();
    assert!(p.valid(&input, true).is_err());
    assert!(
        p.valid(&json!({"session_id":Uuid::new_v4()}), false)
            .is_err()
    );
    p.generation = 0;
    assert!(p.valid(&input, false).is_err());
}

#[test]
fn pending_head_proof_is_explicit_and_cannot_masquerade_as_an_accepted_row() {
    let (mut p, text) = permit("pending whole body\n末尾", 10);
    let q = p.queue.take().unwrap();
    p.head = Some(Frame {
        digest: q.digest,
        bytes: q.bytes,
    });
    assert!(p.queue.is_none());
    assert_eq!(p.pending_proof(), Some((q.digest, q.bytes)));
    let input = json!({"session_id": p.root});
    p.valid(&input, true).unwrap();
    assert!(p.valid(&input, false).is_err());
    frames(
        &format!("<task-notification>{text}</task-notification>"),
        &[p],
    )
    .unwrap();

    let (mut p, _) = permit("original", 10);
    let q = p.queue.as_ref().unwrap();
    p.head = Some(Frame {
        digest: q.digest,
        bytes: q.bytes,
    });
    assert!(p.valid(&json!({"session_id": p.root}), true).is_err());
    assert!(frames("irrelevant", &[p]).is_err());
}

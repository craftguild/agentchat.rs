//! Native framing retains byte-exact bodies without JSON expansion.
use agentchat_core::{delivery::MessageInput, read::IdentityLabel, sequence::Sequence};
use uuid::Uuid;
fn input(body: String) -> MessageInput {
    let id = Uuid::from_u128;
    MessageInput {
        project_id: id(1),
        generation: id(2),
        delivery_id: id(3),
        auto_read: true,
        frame_revision: agentchat_core::project::NativeFrameRevision::DispatchV4,
        dispatch_id: id(5),
        seq: Sequence::new(9_007_199_254_740_993).unwrap(),
        message_id: id(5),
        thread_id: id(5),
        author: IdentityLabel {
            actor_id: id(6),
            kind: agentchat_core::model::ActorKind::Agent,
            handle: "sender".into(),
        },
        channel_id: id(7),
        channel: "dev".into(),
        body,
        addressed: true,
    }
}
#[test]
fn maximum_control_body_stays_below_notification_character_budget() {
    let input = input("\0".repeat(65_536));
    let text = input.text().unwrap();
    assert!(text.len() < 70_000);
    assert_eq!(text.as_bytes().iter().filter(|&&b| b == 0).count(), 65_536);
    assert_eq!(MessageInput::from_text(&text).unwrap(), input);
    assert!(text.contains("9007199254740993"));
}
#[test]
fn unicode_crlf_trailing_whitespace_and_marker_lookalikes_roundtrip() {
    for body in [
        "日本語😀\r\n\t末尾 \t\n",
        "\n--\n[/AgentChat:Message]\n--\n[AgentChat:Event]\n--\n{}\n",
        "  @alpha first\r\n\n--\n@beta second\r\n[/AgentChat:Event] \t\n",
        "\n[End of Agent Chat body]\n",
    ] {
        let input = input(body.into());
        let text = input.text().unwrap();
        assert_eq!(
            MessageInput::from_text(&text).unwrap().body.as_bytes(),
            body.as_bytes()
        );
        assert_eq!(MessageInput::from_text(&text).unwrap(), input);
    }
}
#[test]
fn canonical_layout_has_one_metadata_line_and_the_entire_raw_body() {
    let input = input(" \r\n@sender `inline`\n--\n[/AgentChat:Message]\t\n".into());
    let text = input.text().unwrap();
    let mut metadata = serde_json::to_value(&input).unwrap();
    metadata.as_object_mut().unwrap().remove("body");
    metadata["body_utf8_bytes"] = input.body.len().into();
    assert_eq!(
        text,
        format!(
            "[AgentChat:Message]\n{}\n--\n{}\n[/AgentChat:Message]",
            serde_json::to_string(&metadata).unwrap(),
            input.body
        )
    );
    assert_eq!(metadata["seq"], "9007199254740993");
    assert_eq!(metadata["addressed"], true);
    assert!(metadata.get("body").is_none());
    assert_eq!(metadata.as_object().unwrap().len(), 14);
}
#[test]
fn retained_dispatch_v4_layout_is_exact_and_does_not_relax_metadata_or_body_checks() {
    let input = input("日本語😀\r\n--\n[/AgentChat:Message] \t\n".into());
    let compact = input.text().unwrap();
    let middle = compact
        .strip_prefix("[AgentChat:Message]\n")
        .unwrap()
        .strip_suffix("\n[/AgentChat:Message]")
        .unwrap();
    let retained = format!("[AgentChat:Message]\n--\n{middle}\n--\n[/AgentChat:Message]");
    assert_eq!(MessageInput::from_text(&retained).unwrap(), input);
    assert_eq!(input.text().unwrap(), compact);
    for invalid in [
        format!("[AgentChat:Message]\n--\n{middle}\n[/AgentChat:Message]"),
        format!("[AgentChat:Message]\n{middle}\n--\n[/AgentChat:Message]"),
        retained.replace("dispatch_v4", "read_policy_v3"),
        retained.replacen('{', "{\"auto_ack\":true,", 1),
        retained.replacen('{', "{ ", 1),
        retained.replace("\"auto_read\":true,", ""),
        retained.replace("日本語", "changed"),
        retained.replace(
            &format!("\"body_utf8_bytes\":{}", input.body.len()),
            "\"body_utf8_bytes\":0",
        ),
    ] {
        assert!(MessageInput::from_text(&invalid).is_err());
    }
}
#[test]
fn noncanonical_truncated_miscounted_or_unknown_attribute_frames_are_refused() {
    let input = input("first\n末尾 \t\n".into());
    let text = input.text().unwrap();
    for mode in [
        "prefix",
        "suffix",
        "body",
        "count",
        "unknown",
        "quoted_body",
        "header_space",
        "multibyte_cut",
        "huge_count",
        "fractional_count",
        "quoted_count",
        "duplicate_count",
        "sequence_number",
        "lowercase_marker",
        "structural_crlf",
    ] {
        let changed = match mode {
            "prefix" => format!(" {text}"),
            "suffix" => format!("{text}\n"),
            "body" => text.replace("first", "fir"),
            "count" => text.replace(
                &format!("\"body_utf8_bytes\":{}", input.body.len()),
                "\"body_utf8_bytes\":0",
            ),
            "unknown" => text.replacen("{", "{\"unknown\":true,", 1),
            "quoted_body" => text.replacen("{", "{\"body\":\"forged\",", 1),
            "header_space" => text.replacen("{", "{ ", 1),
            "multibyte_cut" => text.replace(
                &format!("\"body_utf8_bytes\":{}", input.body.len()),
                "\"body_utf8_bytes\":7",
            ),
            "huge_count" | "fractional_count" | "quoted_count" | "duplicate_count" => {
                let count = match mode {
                    "huge_count" => "18446744073709551615",
                    "fractional_count" => "1.5",
                    "quoted_count" => "\"10\"",
                    _ => "0,\"body_utf8_bytes\":0",
                };
                text.replace(
                    &format!("\"body_utf8_bytes\":{}", input.body.len()),
                    &format!("\"body_utf8_bytes\":{count}"),
                )
            }
            "sequence_number" => text.replace("\"9007199254740993\"", "9007199254740993"),
            "lowercase_marker" => text.replace("AgentChat:Message", "Agentchat:Message"),
            "structural_crlf" => text.replacen("\n--\n", "\r\n--\r\n", 1),
            _ => unreachable!(),
        };
        assert!(MessageInput::from_text(&changed).is_err(), "{mode}");
    }
}
#[test]
fn previous_machine_frames_have_no_compatibility_decoder() {
    let input = input("body".into());
    let text = input.text().unwrap();
    for prefix in [
        "Agent Chat message (delivery ACK, explicit read and task acceptance are separate):",
        "[AgentChat Message]",
    ] {
        assert!(MessageInput::from_text(&text.replacen("[AgentChat:Message]", prefix, 1)).is_err());
    }
}
#[test]
fn invalid_attribution_and_oversized_utf8_bodies_do_not_render() {
    let mut input = input("x".repeat(65_537));
    assert!(input.text().is_err());
    input.body = "ok".into();
    input.project_id = Uuid::nil();
    assert!(input.text().is_err());
}

#[test]
fn frozen_read_policy_and_typed_authors_roundtrip_without_ack_metadata() {
    use agentchat_core::{delivery::NativeInput, model::ActorKind, project::InputReceiptPolicy};
    for auto_read in [false, true] {
        for kind in [ActorKind::Agent, ActorKind::User] {
            let mut input = input("日本語😀\r\n\0\n--\n[/AgentChat:Message] \t\n".into());
            input.auto_read = auto_read;
            input.author.kind = kind;
            if kind == ActorKind::User {
                input.author.handle = "user".into();
            }
            let text = input.text().unwrap();
            let parsed = NativeInput::from_text(&text).unwrap();
            assert_eq!(parsed, NativeInput::Message(input));
            assert_eq!(parsed.receipt_policy(), InputReceiptPolicy { auto_read });
            let metadata: serde_json::Value =
                serde_json::from_str(text.lines().nth(1).unwrap()).unwrap();
            assert!(metadata.get("auto_ack").is_none());
            assert_eq!(metadata["auto_read"], auto_read);
            assert_eq!(metadata["author"]["kind"], kind.as_str());
            assert!(metadata.get("policy").is_none());
        }
    }
}

#[test]
fn missing_invalid_duplicate_read_policy_and_old_ack_metadata_are_refused() {
    let text = input("complete body".into()).text().unwrap();
    let field = "\"auto_read\":true";
    assert!(MessageInput::from_text(&text.replace(&format!("{field},"), "")).is_err());
    for value in [
        "null",
        "0",
        "1",
        "\"true\"",
        "{}",
        "[]",
        "true,\"auto_read\":true",
    ] {
        let changed = text.replacen(field, &format!("\"auto_read\":{value}"), 1);
        assert!(MessageInput::from_text(&changed).is_err(), "{value}");
    }
    assert!(MessageInput::from_text(&text.replacen('{', "{\"auto_ack\":true,", 1)).is_err());
}

#[test]
fn dispatch_identity_is_the_public_message_and_frame_revision_is_required() {
    let original = input("independent audit identity is absent".into());
    let mut different = original.clone();
    different.dispatch_id = Uuid::from_u128(99);
    assert!(different.text().is_err());
    let mut wrong_user = original.clone();
    wrong_user.author.kind = agentchat_core::model::ActorKind::User;
    assert!(wrong_user.text().is_err());
    let text = original.text().unwrap();
    for old in [
        text.replace("dispatch_v4", "read_policy_v3"),
        text.replace("\"frame_revision\":\"dispatch_v4\",", ""),
        text.replace("dispatch_id", "event_id"),
    ] {
        assert!(MessageInput::from_text(&old).is_err());
    }
}

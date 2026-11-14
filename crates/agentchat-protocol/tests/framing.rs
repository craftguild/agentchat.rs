//! T13/T17 foundation: bounded framing before allocation and explicit handshake.

use agentchat_core::ErrorCode;
use agentchat_protocol::{
    envelope::{Envelope, Handshake},
    framing::{self, MAX_FRAME_BYTES},
};
use serde_json::{Value, json};
use tokio::io::BufReader;
use uuid::Uuid;

#[tokio::test]
async fn finite_frames_do_not_consume_the_next_frame() {
    let input = b"{\"seq\":\"9223372036854775807\"}\n{\"ok\":true}\n";
    let mut reader = BufReader::with_capacity(3, &input[..]);
    assert_eq!(
        framing::read::<_, Value>(&mut reader).await.unwrap(),
        Some(json!({"seq":"9223372036854775807"}))
    );
    assert_eq!(
        framing::read::<_, Value>(&mut reader).await.unwrap(),
        Some(json!({"ok":true}))
    );
    assert!(
        framing::read::<_, Value>(&mut reader)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn malformed_truncated_and_oversized_frames_are_rejected() {
    for bytes in [
        b"{}".to_vec(),
        b"\xff\n".to_vec(),
        b"{]\n".to_vec(),
        vec![b'a'; MAX_FRAME_BYTES + 1],
    ] {
        let mut reader = BufReader::with_capacity(7, bytes.as_slice());
        assert_eq!(
            framing::read::<_, Value>(&mut reader)
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidArgument
        );
    }
    let exact = format!("\"{}\"\n", "x".repeat(MAX_FRAME_BYTES - 3));
    let mut reader = BufReader::with_capacity(11, exact.as_bytes());
    assert!(
        framing::read::<_, Value>(&mut reader)
            .await
            .unwrap()
            .is_some()
    );
    let mut output = Vec::new();
    framing::write(&mut output, &"x".repeat(MAX_FRAME_BYTES - 3))
        .await
        .unwrap();
    assert_eq!(output.len(), MAX_FRAME_BYTES);
    output.clear();
    assert!(
        framing::write(&mut output, &"x".repeat(MAX_FRAME_BYTES - 2))
            .await
            .is_err()
    );
    assert!(output.is_empty());
}

#[test]
fn handshake_and_result_identity_are_explicit() {
    let project = Uuid::new_v4();
    assert!(
        Handshake {
            version: 1,
            project_id: project
        }
        .validate(project)
        .is_ok()
    );
    assert_eq!(
        Handshake {
            version: 2,
            project_id: project
        }
        .validate(project)
        .unwrap_err()
        .code,
        ErrorCode::UnsupportedProtocol
    );
    assert_eq!(
        Handshake {
            version: 1,
            project_id: project
        }
        .validate(Uuid::new_v4())
        .unwrap_err()
        .code,
        ErrorCode::ProjectBindingMismatch
    );
    let request_id = Uuid::new_v4();
    let envelope = Envelope::new(request_id, Some(project), Ok(json!({"ok":true})));
    let encoded = serde_json::to_value(envelope).unwrap();
    assert_eq!(encoded["result"], json!({"ok":true}));
    assert!(encoded.get("error").is_none());
}

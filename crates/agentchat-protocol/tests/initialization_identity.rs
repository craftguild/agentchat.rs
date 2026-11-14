//! Host-only initializer identity cannot be manufactured through public JSON.
use agentchat_core::{ErrorCode, model::HostKind};
use agentchat_protocol::operations::{InitializationIdentity, Target};
use serde_json::json;

#[test]
fn native_initializer_metadata_is_bounded_and_never_accepted_from_wire_arguments() {
    for invalid in [String::new(), "x".repeat(1025)] {
        assert_eq!(
            InitializationIdentity::new(HostKind::Claude, invalid)
                .unwrap_err()
                .code,
            ErrorCode::SessionRequired
        );
    }
    let identity =
        InitializationIdentity::new(HostKind::Claude, "exact-native-root".into()).unwrap();
    assert_eq!(identity.host(), HostKind::Claude);
    assert_eq!(identity.host_session_id(), "exact-native-root");
    let target = Target {
        initialization_identity: Some(identity),
        ..Target::default()
    };
    let encoded = serde_json::to_value(target).unwrap();
    assert!(encoded.get("initialization_identity").is_none());
    assert!(
        serde_json::from_value::<Target>(
            json!({"initialization_identity":{"host":"claude","host_session_id":"borrowed-root"}})
        )
        .is_err()
    );
    let ordinary: Target =
        serde_json::from_value(json!({"project_path":"/explicit","context_id":null})).unwrap();
    assert!(ordinary.initialization_identity.is_none());
}

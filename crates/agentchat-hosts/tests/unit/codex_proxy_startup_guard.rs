use super::*;
use agentchat_core::model::Context;
use std::collections::BTreeMap;
use uuid::Uuid;

#[tokio::test]
async fn initial_release_cannot_replace_a_tracked_binding_fence() {
    let home = tempfile::tempdir().unwrap();
    let mut child = crate::launch::NativeChild::spawn(
        Path::new("/bin/sleep"),
        home.path(),
        &["30".into()],
        &BTreeMap::new(),
    )
    .await
    .unwrap();
    let proof = child.proof();
    let backend = home.path().join("backend.sock");
    let guard = Guard::new(home.path(), &backend, Some(proof.clone()));
    let binding = Binding::new(
        Context {
            context_id: Uuid::new_v4(),
            incarnation: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            host: HostKind::Codex,
            host_session_id: Uuid::new_v4().to_string(),
            host_version: "synthetic-startup".into(),
            os_profile: "synthetic".into(),
            working_directory: home.path().into(),
        },
        proof.identity().clone(),
        Endpoint::Codex {
            socket: backend,
            executable: proof.owner().executable(),
        },
    )
    .unwrap();
    // Model the already-tracked state without publishing any launch authority.
    guard.tracked.lock().unwrap().binding = Some(binding);
    let (sender, mut receiver) = tokio::sync::oneshot::channel();
    let mut fence = crate::codex::ProxyFence::new(sender);
    assert!(guard.release_initial(&mut fence).is_err());
    assert!(matches!(
        receiver.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    child.terminate().await.unwrap();
}

//! Explicit actual native daemon preparation; no thread, model or membership.
use agentchat_hosts::codex::{Client, Daemon};

#[tokio::test]
#[ignore = "manual native daemon start/reuse; explicit AGENTCHAT_CODEX_PROBE_CLI, no thread/model"]
async fn actual_shared_daemon_preparation_and_connection() {
    let cli = std::path::PathBuf::from(
        std::env::var_os("AGENTCHAT_CODEX_PROBE_CLI").expect("explicit native probe CLI"),
    );
    let daemon = Daemon::ensure_running(&cli).await.unwrap();
    let client = Client::connect(&daemon.socket, &daemon.version)
        .await
        .unwrap();
    assert_eq!(client.version(), daemon.version);
    println!(
        "NATIVE_DAEMON_PREPARED version={} executable={}",
        daemon.version,
        daemon.executable.display()
    );
}

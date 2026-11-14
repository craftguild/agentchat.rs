//! Native physical-image fixture and explicitly synthetic local API server.
use super::*;
use std::{os::unix::net::UnixListener, path::PathBuf, process::Command, sync::OnceLock};

fn compiled() -> &'static PathBuf {
    static IMAGE: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
    &IMAGE
        .get_or_init(|| {
            let root = tempfile::tempdir().unwrap();
            let output = root.path().join("native-codex");
            let status = Command::new("rustc")
                .args(["--edition=2024", "-o"])
                .arg(&output)
                .arg(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/fixtures/codex_daemon.rs"
                ))
                .status()
                .unwrap();
            assert!(status.success());
            (root, output)
        })
        .1
}

pub(super) struct Fixture {
    pub root: tempfile::TempDir,
    pub setup: ToolSetup,
    pub cli: PathBuf,
    pub socket: PathBuf,
    pub config: PathBuf,
}
impl Fixture {
    pub fn new(status: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let cli = root.path().join("codex");
        fs::copy(compiled(), &cli).unwrap();
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).unwrap();
        let socket = root.path().join("server.sock");
        // Native daemon discovery requires an existing canonical socket even
        // when a test stops before exercising the synthetic API connection.
        drop(UnixListener::bind(&socket).unwrap());
        let running = json!({"status":"running", "managedCodexPath":cli,
            "managedCodexVersion":"0.163.0", "appServerVersion":"0.163.0", "socketPath":socket});
        let mut probe = running.clone();
        probe["status"] = json!(status);
        fs::write(cli.with_extension("probe"), probe.to_string()).unwrap();
        fs::write(cli.with_extension("running"), running.to_string()).unwrap();
        let config = root.path().join("config");
        let setup = ToolSetup::new(
            root.path().join("state"),
            root.path().join("agentchat"),
            crate::setup::Environment {
                user_config_root: Some(config.clone()),
                ..Default::default()
            },
            Some(root.path().as_os_str().into()),
        );
        Self {
            root,
            setup,
            cli,
            socket,
            config,
        }
    }
    pub async fn tools(&self, request: Uuid, project: Uuid) -> serde_json::Value {
        fs::create_dir_all(self.config.join(".codex")).unwrap();
        fs::write(
            self.config.join(".codex/config.toml"),
            "approval_policy='on-request'\n[mcp_servers.foreign]\ncommand='retained'\n",
        )
        .unwrap();
        self.setup.install(request, project).await.unwrap()
    }
    pub fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.cli.with_extension("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
    pub fn starts(&self) -> usize {
        self.calls()
            .iter()
            .filter(|s| *s == "app-server daemon start")
            .count()
    }
    pub fn api(&self, reject: bool) -> tokio::task::JoinHandle<()> {
        fs::remove_file(&self.socket).unwrap();
        let listener = tokio::net::UnixListener::bind(&self.socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) =
                tokio::time::timeout(std::time::Duration::from_secs(10), listener.accept())
                    .await
                    .unwrap()
                    .unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let text = socket.next().await.unwrap().unwrap().into_text().unwrap();
            let request: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(request["method"], "initialize");
            let response = if reject {
                json!({"id":request["id"],"error":{"code":-32601,"message":"missing"}})
            } else {
                json!({"id":request["id"],"result":{"userAgent":"codex_cli_rs/0.163.0 (fixture)"}})
            };
            socket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    response.to_string().into(),
                ))
                .await
                .unwrap();
            if !reject {
                let notification: serde_json::Value = serde_json::from_str(
                    &socket.next().await.unwrap().unwrap().into_text().unwrap(),
                )
                .unwrap();
                assert_eq!(notification["method"], "initialized");
            }
        })
    }
}

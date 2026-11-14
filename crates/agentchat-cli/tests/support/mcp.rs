//! Actual SDK stdio subprocess client shared by executable integration tests.
use super::Fixture;
use agentchat_protocol::framing::{self, FramedReader};
use serde_json::{Value, json};
use std::{process::Stdio, time::Duration};
use tokio::{
    io::BufReader,
    process::{Child, ChildStdin, ChildStdout},
};
use uuid::Uuid;

pub struct Mcp {
    child: Child,
    input: ChildStdin,
    output: FramedReader<BufReader<ChildStdout>>,
    id: u64,
}
impl Mcp {
    pub fn process_id(&self) -> u32 {
        self.child.id().expect("owned MCP process is unreaped")
    }
    pub async fn start(f: &Fixture) -> Self {
        // Even an inherited exact descriptor must not implicitly bind a new
        // globally installed MCP process. Native restoration uses the private
        // verified attachment path; this fixture starts explicitly unbound.
        let child = f
            .command(&["mcp"])
            .env("AGENTCHAT_CONTEXT", Uuid::new_v4().to_string())
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        Self::from_child(child).await
    }
    pub async fn from_child(mut child: Child) -> Self {
        let input = child.stdin.take().unwrap();
        let output = FramedReader::new(BufReader::new(child.stdout.take().unwrap()));
        let mut session = Self {
            child,
            input,
            output,
            id: 0,
        };
        let initialized = session
            .request(
                "initialize",
                json!({
                    "protocolVersion":"2025-11-25", "capabilities":{},
                    "clientInfo":{"name":"isolated-test","version":"1"}
                }),
            )
            .await;
        assert_eq!(
            initialized["result"]["protocolVersion"], "2025-11-25",
            "{initialized}"
        );
        framing::write(
            &mut session.input,
            &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        )
        .await
        .unwrap();
        session
    }
    pub async fn request(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        framing::write(
            &mut self.input,
            &json!({"jsonrpc":"2.0","id":self.id,"method":method,"params":params}),
        )
        .await
        .unwrap();
        let response: Value = tokio::time::timeout(Duration::from_secs(8), self.output.read())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(response["id"], self.id, "{response}");
        response
    }
    pub async fn call(&mut self, name: &str, arguments: Value) -> Value {
        let response = self
            .request("tools/call", json!({"name":name,"arguments":arguments}))
            .await;
        assert!(response.get("error").is_none(), "{response}");
        let result = &response["result"];
        let envelope = result["structuredContent"].clone();
        assert_eq!(
            result["isError"],
            envelope.get("error").is_some(),
            "{response}"
        );
        assert_eq!(envelope["version"], 1, "{response}");
        envelope
    }
    pub async fn close(self) {
        drop(self.input);
        let output = tokio::time::timeout(Duration::from_secs(8), self.child.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
    }
}

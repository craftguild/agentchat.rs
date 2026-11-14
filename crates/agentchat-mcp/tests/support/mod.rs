//! SDK protocol fixtures; this backend is synthetic and admits no real host.
#![allow(dead_code)]
use agentchat_core::{Error, ErrorCode, Result, application::Mutation};
use agentchat_mcp::Server;
use agentchat_protocol::{
    envelope::Envelope,
    operations::{Attachment, Backend, Binding, Operation, Target},
};
use async_trait::async_trait;
use rmcp::{
    RoleClient, ServiceExt,
    model::{CallToolRequestParams, CallToolResult, ClientConfig, ProtocolVersion},
    service::RunningService,
};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::{
    sync::{Notify, Semaphore},
    task::JoinHandle,
};
use uuid::Uuid;

pub struct Fake {
    pub project: Uuid,
    pub agent: Uuid,
    pub calls: Mutex<Vec<(Uuid, Target, Operation)>>,
    pub failures: AtomicUsize,
    pub attach_fails: AtomicBool,
    pub attached: Arc<AtomicUsize>,
    pub block_join: AtomicBool,
    pub entered: Notify,
    pub release: Semaphore,
    pub result: Mutex<Option<Value>>,
}
impl Default for Fake {
    fn default() -> Self {
        Self {
            project: Uuid::new_v4(),
            agent: Uuid::new_v4(),
            calls: Mutex::default(),
            failures: AtomicUsize::new(0),
            attach_fails: AtomicBool::new(false),
            attached: Arc::default(),
            block_join: AtomicBool::new(false),
            entered: Notify::new(),
            release: Semaphore::new(0),
            result: Mutex::default(),
        }
    }
}
struct Held {
    binding: Binding,
    count: Arc<AtomicUsize>,
}
impl Drop for Held {
    fn drop(&mut self) {
        self.count.fetch_sub(1, Ordering::SeqCst);
    }
}
#[async_trait]
impl Attachment for Held {
    fn binding(&self) -> &Binding {
        &self.binding
    }
    async fn check(&self) -> Result<()> {
        Ok(())
    }
    async fn refresh(&self) -> Result<()> {
        Ok(())
    }
}
#[async_trait]
impl Backend for Fake {
    async fn attach(&self, target: Target) -> Result<Box<dyn Attachment>> {
        if self.attach_fails.load(Ordering::SeqCst) {
            return Err(Error::new(
                ErrorCode::NotJoined,
                "Synthetic unjoined context.",
            ));
        }
        self.attached.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Held {
            binding: Binding {
                project_id: self.project,
                context_id: target.context_id.unwrap(),
                agent_id: self.agent,
            },
            count: self.attached.clone(),
        }))
    }
    async fn invoke(&self, id: Uuid, target: Target, operation: Operation) -> Envelope<Value> {
        self.calls
            .lock()
            .unwrap()
            .push((id, target, operation.clone()));
        if self
            .failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Envelope::new(
                id,
                Some(self.project),
                Err(Error::new(ErrorCode::IoError, "Synthetic lost response.")),
            );
        }
        let joining = matches!(
            operation,
            Operation::Mutate {
                command: Mutation::Join { .. }
            }
        );
        if joining && self.block_join.load(Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.acquire().await.unwrap().forget();
        }
        let result = self.result.lock().unwrap().clone().unwrap_or_else(|| {
            if joining {
                json!({"agent_id":self.agent})
            } else {
                json!({"accepted":true})
            }
        });
        Envelope::new(id, Some(self.project), Ok(result))
    }
}
pub struct Session {
    pub server: Server,
    pub client: RunningService<RoleClient, ClientConfig>,
    pub task: JoinHandle<Result<()>>,
}
impl Session {
    pub async fn new(fake: Arc<Fake>) -> Self {
        Self::from_server(Server::new(fake)).await
    }
    pub async fn from_server(server: Server) -> Self {
        let (a, b) = tokio::io::duplex(8192);
        let (reader, writer) = tokio::io::split(b);
        let owned = server.clone();
        let task = tokio::spawn(agentchat_mcp::serve(owned, reader, writer));
        let mut info = ClientConfig::default();
        info.protocol_version = ProtocolVersion::V_2025_11_25;
        let client = info.serve(a).await.unwrap();
        Self {
            server,
            client,
            task,
        }
    }
    pub async fn call(&self, name: &'static str, arguments: Value) -> CallToolResult {
        self.client
            .call_tool(params(name, arguments))
            .await
            .unwrap()
    }
    pub async fn join(&self, context: Uuid) -> Value {
        envelope(
            self.call("agentchat_join", json!({"context_id":context}))
                .await,
        )
    }
    pub async fn close(self) {
        self.client.cancel().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(8), self.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}
pub fn params(name: &'static str, value: Value) -> CallToolRequestParams {
    CallToolRequestParams::new(name).with_arguments(value.as_object().unwrap().clone())
}
pub fn envelope(result: CallToolResult) -> Value {
    let value = result.structured_content.unwrap();
    assert_eq!(result.is_error, Some(value.get("error").is_some()));
    assert_eq!(value["version"], 1);
    assert_ne!(value.get("result").is_some(), value.get("error").is_some());
    value
}

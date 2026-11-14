//! JSON-RPC validation and bounded finite batches, separate from business policy.

use super::coordinator::{Response, Service, Subscription};
use agentchat_core::{Error, ErrorCode, Result};
use agentchat_protocol::{
    framing::MAX_FRAME_BYTES,
    rpc::{Call, Id, ReplyBody, Request, TypedReply as RpcReply},
};
use serde_json::Value;
use uuid::Uuid;

type Reply = RpcReply<Response>;
const MAX_BATCH: usize = 32;
pub(super) struct Session {
    pub service: Service,
    pub connection: Uuid,
    project: Uuid,
    bound: Option<Uuid>,
}
impl Session {
    pub fn new(service: Service, connection: Uuid, project: Uuid) -> Self {
        Self {
            service,
            connection,
            project,
            bound: None,
        }
    }
    pub async fn execute(&mut self, value: Value) -> (Option<Value>, Option<Subscription>) {
        let Value::Array(values) = value else {
            let (reply, subscription) = self.one(value, false).await;
            return (reply.map(encode), subscription);
        };
        if values.is_empty() || values.len() > MAX_BATCH {
            return (
                Some(encode(failure(
                    None,
                    -32600,
                    "RPC batches require 1 through 32 entries.",
                ))),
                None,
            );
        }
        let mut replies = Vec::new();
        let mut bytes = 2;
        for value in values {
            if let (Some(mut reply), _) = self.one(value, true).await {
                let mut size = serde_json::to_vec(&reply).map_or(MAX_FRAME_BYTES, |v| v.len());
                // Keep room for every remaining bounded error. Evaluate one
                // result at a time: a batch cannot collect 32 large pages.
                if bytes + size + 1 > MAX_FRAME_BYTES - MAX_BATCH * 2048 {
                    let mut error = Error::new(
                        ErrorCode::InvalidArgument,
                        "Batch response budget exceeded; request finite pages separately.",
                    );
                    if let ReplyBody::Success { result } = &reply.body {
                        error.details = serde_json::json!({"request_processed":true,"request_id":result.request_id});
                    }
                    reply = Reply::failure(reply.id, -32000, error);
                    size = serde_json::to_vec(&reply).map_or(2048, |v| v.len());
                }
                bytes += size + 1;
                replies.push(encode(reply));
            }
        }
        (
            if replies.is_empty() {
                None
            } else {
                Some(Value::Array(replies))
            },
            None,
        )
    }
    async fn one(&mut self, value: Value, batch: bool) -> (Option<Reply>, Option<Subscription>) {
        let request = match parse(value) {
            Ok(r) => r,
            Err(reply) => return (Some(*reply), None),
        };
        let call = match decode(&request) {
            Ok(c) => c,
            Err(reply) => return (request.id.is_some().then_some(*reply), None),
        };
        if batch && call.dedicated() {
            return (
                request.id.map(|id| {
                    failure(
                        Some(id),
                        -32602,
                        "Event subscription requires a dedicated request frame.",
                    )
                }),
                None,
            );
        }
        let key = request.result_key(&call);
        let context = call.context_id();
        match self
            .service
            .call(self.connection, Some(key), call, self.bound)
            .await
        {
            Ok(answer) => {
                if answer.bind {
                    self.bound = context.or(self.bound);
                }
                (
                    request
                        .id
                        .map(|id| Reply::success(Some(id), key, self.project, answer.result)),
                    answer.subscription,
                )
            }
            Err(error) => (
                request.id.map(|id| Reply::failure(Some(id), -32000, error)),
                None,
            ),
        }
    }
}
fn encode(reply: Reply) -> Value {
    // All fields are JSON values, UUIDs, strings and ordinary finite integers.
    serde_json::to_value(reply).expect("RPC reply fields are JSON-serializable")
}
fn failure(id: Option<Id>, code: i32, message: &str) -> Reply {
    Reply::failure(id, code, Error::new(ErrorCode::InvalidArgument, message))
}
pub(super) fn reply(id: Option<Id>, key: Uuid, project: Uuid, result: Result<Value>) -> Reply {
    match result {
        Ok(result) => Reply::success(id, key, project, result.into()),
        Err(error) => Reply::failure(id, -32000, error),
    }
}
pub(super) fn parse(value: Value) -> std::result::Result<Request, Box<Reply>> {
    let request = serde_json::from_value::<Request>(value)
        .map_err(|_| Box::new(failure(None, -32600, "Invalid JSON-RPC request object.")))?;
    if request.jsonrpc != "2.0" {
        return Err(Box::new(failure(
            None,
            -32600,
            "Unsupported JSON-RPC version.",
        )));
    }
    Ok(request)
}
pub(super) fn decode(request: &Request) -> std::result::Result<Call, Box<Reply>> {
    if !Call::known(&request.method) {
        return Err(Box::new(failure(
            request.id.clone(),
            -32601,
            "Unknown local RPC method.",
        )));
    }
    request
        .call()
        .map_err(|error| Box::new(Reply::failure(request.id.clone(), -32602, error)))
}

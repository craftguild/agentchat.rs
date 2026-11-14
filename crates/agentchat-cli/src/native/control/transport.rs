//! Bounded same-user native controls; receipts are not delivery ACKs or reads.
use crate::claude_gateway::records::{Control, Endpoint, Request, Response};
use agentchat_core::{Error, ErrorCode, Result};
use agentchat_hosts::claude::namespace::OwnerNamespace;
use agentchat_protocol::framing;
use std::time::Duration;
use tokio::{io::BufReader, net::UnixStream};

pub(super) async fn endpoint(namespace: &OwnerNamespace) -> Result<Option<Endpoint>> {
    namespace.transport().await
}
pub(super) async fn send(
    namespace: &OwnerNamespace,
    control: Control,
    missing_ok: bool,
) -> Result<()> {
    let Some(endpoint) = endpoint(namespace).await? else {
        return if missing_ok { Ok(()) } else { Err(required()) };
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        let stream = match UnixStream::connect(&endpoint.socket).await {
            Ok(stream) => stream,
            Err(e)
                if missing_ok
                    && matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    ) =>
            {
                return Ok(());
            }
            Err(_) => return Err(required()),
        };
        if stream.peer_cred().map(|c| c.uid()).ok() != Some(rustix::process::geteuid().as_raw()) {
            return Err(required());
        }
        let (reader, mut writer) = stream.into_split();
        framing::write(
            &mut writer,
            &Request {
                nonce: endpoint.nonce,
                control,
            },
        )
        .await
        .map_err(|_| required())?;
        let response: Response = framing::read(&mut BufReader::new(reader))
            .await
            .map_err(|_| required())?
            .ok_or_else(required)?;
        if !response.accepted {
            let code = response.error.unwrap_or(ErrorCode::SessionRequired);
            let mut error = Error::new(code, "The exact native callback control was refused.");
            error.retryable = code == ErrorCode::Busy;
            return Err(error);
        }
        Ok(())
    })
    .await
    .map_err(|_| required())?
}
pub(super) fn required() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "The current native callback or transport changed.",
    )
}

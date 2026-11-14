//! Strict JSON-RPC parsing, initial handshake and immutable connection binding.

use super::{
    RuntimeOptions,
    activity::{Activity, Kind},
    coordinator::Service,
    requests::{Session, decode, parse, reply},
};
use agentchat_core::{Error, ErrorCode, Result};
use agentchat_protocol::{
    framing,
    rpc::{Call, Reply},
};
use serde_json::{Value, json};
use tokio::{
    io::BufReader,
    net::{UnixStream, unix::OwnedWriteHalf},
    sync::watch,
    time::timeout,
};
use uuid::Uuid;

pub(super) async fn write(
    writer: &mut OwnedWriteHalf,
    value: &impl serde::Serialize,
    options: RuntimeOptions,
) -> Result<()> {
    timeout(options.io_timeout, framing::write(writer, value))
        .await
        .map_err(|_| super::unavailable("Runtime response exceeded its write deadline."))?
}
pub(super) async fn serve(
    stream: UnixStream,
    service: Service,
    activity: Activity,
    project: Uuid,
    instance: Uuid,
    options: RuntimeOptions,
    mut shutdown: watch::Receiver<bool>,
) {
    if stream.peer_cred().map(|cred| cred.uid()).ok() != Some(rustix::process::geteuid().as_raw()) {
        return;
    }
    let connection = Uuid::new_v4();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let first = timeout(options.io_timeout, framing::read::<_, Value>(&mut reader)).await;
    let value = match first {
        Ok(Ok(Some(value))) => value,
        Ok(Err(error)) => {
            let _ = write(&mut writer, &Reply::failure(None, -32700, error), options).await;
            return;
        }
        _ => return,
    };
    let request = match parse(value) {
        Ok(request) => request,
        Err(reply) => {
            let _ = write(&mut writer, &reply, options).await;
            return;
        }
    };
    let call = match decode(&request) {
        Ok(call) => call,
        Err(reply) => {
            if request.id.is_some() {
                let _ = write(&mut writer, &reply, options).await;
            }
            return;
        }
    };
    let result = match call {
        Call::Handshake(hello) => hello
            .validate(project)
            .map(|_| json!({"version":1,"project_id":project,"instance_id":instance})),
        _ => Err(Error::new(
            ErrorCode::UnsupportedProtocol,
            "Handshake must precede every local command.",
        )),
    };
    let valid = result.is_ok();
    if request.id.is_some() {
        let key = request.result_key(&Call::Handshake(agentchat_protocol::envelope::Handshake {
            version: 1,
            project_id: project,
        }));
        let reply = reply(request.id, key, project, result);
        if write(&mut writer, &reply, options).await.is_err() {
            return;
        }
    }
    if !valid {
        return;
    }
    let mut session = Session::new(service, connection, project);
    loop {
        let incoming = tokio::select! {
            _=shutdown.changed()=>break,
            incoming=framing::read::<_,Value>(&mut reader)=>incoming,
        };
        let value = match incoming {
            Ok(Some(value)) => value,
            Ok(None) => break,
            Err(error) => {
                let _ = write(&mut writer, &Reply::failure(None, -32700, error), options).await;
                break;
            }
        };
        let request_guard = activity.enter(Kind::Request);
        let (response, subscription) = session.execute(value).await;
        if let Some(response) = response
            && write(&mut writer, &response, options).await.is_err()
        {
            break;
        }
        if let Some(subscription) = subscription {
            let _observer = activity.enter(Kind::Observer);
            drop(request_guard);
            let _ = super::follow::run(
                &mut reader,
                &mut writer,
                &session.service,
                connection,
                subscription,
                options,
                shutdown.clone(),
            )
            .await;
            break;
        }
    }
    session.service.gone(connection).await;
}

//! Retain one exact native startup descriptor before readiness publication.
use super::{Operations, Route};
use crate::{
    launch::{authorize_codex_start, prepare_codex_start},
    operations::activation::{Control, Native},
};
use agentchat_core::{ErrorCode, Result};
use agentchat_hosts::{
    codex::{Daemon, ProxyEvents},
    launch::NativeProof,
};
use std::{path::PathBuf, time::Duration};

pub(super) async fn start(
    operations: &Operations,
    route: &Route,
    proof: NativeProof,
    daemon: Daemon,
    mut proxy: ProxyEvents,
    selection: (String, PathBuf),
    name: Option<String>,
) -> Result<Control> {
    let (root, directory) = selection;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut initial_forwarded = false;
    let prepared = loop {
        // No participation exists yet. Let the first native resume enter while
        // retaining its correlated event for Control before any managed Join.
        let readiness = prepare_codex_start(
            &operations.manager,
            &route.project,
            &proof,
            &daemon,
            &root,
            &directory,
        );
        tokio::pin!(readiness);
        let result = loop {
            tokio::select! {
                result = proxy.prepare_initial(), if !initial_forwarded => {
                    result?;
                    initial_forwarded = true;
                    // Retain this readiness attempt rather than cancelling a
                    // context preparation that may already have reached disk.
                },
                result = &mut readiness => break result,
            }
        };
        match result {
            Ok(prepared) => break prepared,
            Err(error)
                if (error.retryable
                    || matches!(
                        error.code,
                        ErrorCode::RuntimeUnavailable | ErrorCode::IoError
                    ))
                    && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(error) => return Err(error),
        }
    };
    let (socket, ready) =
        authorize_codex_start(&operations.manager, &route.project, &proof, &prepared, name).await?;
    let mut control = Control::new(socket, ready, Native::Codex { proxy });
    // Physical readiness already owns the native lifetime. A fenced initial
    // intent stays in Control and later ticks; it grants no participation yet.
    control.activate_initial(operations, route, &proof).await?;
    Ok(control)
}

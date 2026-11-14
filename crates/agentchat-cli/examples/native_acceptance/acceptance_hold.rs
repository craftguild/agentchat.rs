//! Explicit whole-runtime stop after real native acceptance, before core commit.
use agentchat_core::{
    Error, ErrorCode, Result,
    delivery::{MessageDispatch, MessageProgress},
    model::Context,
};
use agentchat_hosts::claude::metadata;
use serde_json::json;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};

pub(super) struct AcceptanceHold {
    path: PathBuf,
    claimed: AtomicBool,
}
impl AcceptanceHold {
    pub(super) fn new(path: PathBuf, home: &std::path::Path) -> Result<Self> {
        if !path.is_absolute() || path.parent() != Some(home) || path.exists() {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Acceptance hold requires a new file directly in the explicit test state root.",
            ));
        }
        Ok(Self {
            path,
            claimed: AtomicBool::new(false),
        })
    }
    pub(super) fn observe(
        &self,
        context: &Context,
        dispatch: &MessageDispatch,
        progress: &MessageProgress,
    ) -> Result<()> {
        if !matches!(progress, MessageProgress::Accepted(_))
            || self.claimed.swap(true, Ordering::SeqCst)
        {
            return Ok(());
        }
        metadata::write(
            &self.path,
            &json!({
                "context":context,"token":dispatch.token,"generation":dispatch.input.generation,
                "message_id":dispatch.input.message_id,"delivery_id":dispatch.input.delivery_id,
                "seq":dispatch.input.seq,"attempt":dispatch.attempt,"receipt_only":dispatch.awaiting_acceptance,
                "native_accepted":true,"core_completion_held":true,"admitted":false,
            }),
        )?;
        // Stopping only this explicit disposable AGENTCHAT runtime prevents a timeout
        // commit. The driver can prove this process is stopped, then
        // kill/restart it at the durable cut. No native CLI receives a signal.
        rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::STOP)
            .map_err(|_| {
                Error::new(
                    ErrorCode::IoError,
                    "The explicit acceptance runtime could not stop at its cut.",
                )
            })?;
        Ok(())
    }
}

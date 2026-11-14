//! Opaque preparation stages keep Processing before observation and claims before I/O.
use super::{Application, native::NativeReceiptPoll};
use crate::{
    Result,
    delivery::{InputCapabilities, NativeDispatch},
    model::Context,
    ports::{HostState, Transaction},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

/// One caller lifetime for preparatory writes; actual completions never use it.
#[derive(Debug)]
pub struct NativePreparationCancellation(Arc<AtomicBool>);
impl Drop for NativePreparationCancellation {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
#[derive(Debug, Clone)]
pub(super) struct Admission {
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
}
impl Admission {
    pub(super) fn check(value: Option<&Self>) -> Result<()> {
        if value
            .is_some_and(|v| v.cancelled.load(Ordering::Acquire) || Instant::now() >= v.deadline)
        {
            return Err(crate::Error::new(
                crate::ErrorCode::RuntimeUnavailable,
                "Native preparation caller expired before mutation.",
            ));
        }
        Ok(())
    }
}

/// Exact selection read by P, including an explicitly empty original stream.
#[derive(Debug, Clone, Default)]
pub(super) struct Selection {
    pub generation: Option<uuid::Uuid>,
    pub delivery: Option<(uuid::Uuid, crate::sequence::Sequence)>,
}
pub(super) struct ProcessingEffect {
    pub changed: bool,
    pub selection: Selection,
}
impl ProcessingEffect {
    pub fn unchanged(generation: Option<uuid::Uuid>) -> Self {
        Self {
            changed: false,
            selection: Selection {
                generation,
                delivery: None,
            },
        }
    }
}

/// Local mechanism/context validation, before any native state observation.
#[derive(Debug, Clone)]
pub struct NativeProcessing {
    context: Context,
    capabilities: InputCapabilities,
    admission: Option<Admission>,
}
impl NativeProcessing {
    /// Bound a runtime caller to its existing total prepare deadline. Dropping
    /// the returned ticket cancels unapplied P/H work, never an actual result.
    pub fn until(mut self, deadline: Instant) -> (Self, NativePreparationCancellation) {
        let cancelled = Arc::new(AtomicBool::new(false));
        self.admission = Some(Admission {
            cancelled: cancelled.clone(),
            deadline,
        });
        (self, NativePreparationCancellation(cancelled))
    }

    /// Exact original conversation used to preserve ready-queue stream fences.
    pub fn context(&self) -> &Context {
        &self.context
    }
}
/// Processing bookkeeping has returned from its outer durability boundary.
#[derive(Debug)]
pub struct ProcessingReady {
    selection: Selection,
    context: Context,
    capabilities: InputCapabilities,
    changed: bool,
    admission: Option<Admission>,
}
impl ProcessingReady {
    pub(crate) fn changed(&self) -> bool {
        self.changed
    }
}
/// Ordinary observed state, still requiring fresh authority under the writer.
#[derive(Debug, Clone)]
pub struct NativeClaim {
    pub(super) selection: Selection,
    pub(super) context: Context,
    pub(super) capabilities: InputCapabilities,
    pub(super) host: HostState,
    pub(super) admission: Option<Admission>,
}
impl NativeClaim {
    /// Exact original conversation; captured facts never grant another root.
    pub fn context(&self) -> &Context {
        &self.context
    }
}
/// Original receipt work or a fresh, as-yet-uncommitted claim request.
#[derive(Debug)]
pub enum NativePreparation {
    /// A current original receipt poll, or no work; never a new native claim.
    Original(Option<NativeDispatch>),
    /// Fresh captured facts that grant no write or native admission by themselves.
    Claim(NativeClaim),
}
/// A preparation result is released only after the enclosing commit succeeds.
#[derive(Debug)]
pub struct ClaimedNative {
    dispatch: Option<NativeDispatch>,
    changed: bool,
}
impl ClaimedNative {
    /// Take the exact committed dispatch, or the unchanged/expiry result.
    pub fn into_dispatch(self) -> Option<NativeDispatch> {
        self.dispatch
    }
    pub(crate) fn changed(&self) -> bool {
        self.changed
    }
    pub(super) fn none(changed: bool) -> Self {
        Self {
            dispatch: None,
            changed,
        }
    }
    pub(super) fn some(dispatch: NativeDispatch) -> Self {
        Self {
            dispatch: Some(dispatch),
            changed: true,
        }
    }
}
impl Application {
    /// Validate local declarations without observing host controls before Processing.
    pub fn native_processing(&self, context: &Context) -> Result<NativeProcessing> {
        self.validate_context(context)?;
        Ok(NativeProcessing {
            context: context.clone(),
            capabilities: self.host.input_capabilities(context)?,
            admission: None,
        })
    }
    /// Run original receipt and host-state observations only after Processing returns.
    pub async fn observe_native(&self, prepared: ProcessingReady) -> Result<NativePreparation> {
        let ProcessingReady {
            selection,
            context,
            capabilities,
            admission,
            ..
        } = prepared;
        Admission::check(admission.as_ref())?;
        match self.awaiting_native(&context, capabilities).await? {
            NativeReceiptPoll::Ready(value) => {
                return Ok(NativePreparation::Original(Some(*value)));
            }
            NativeReceiptPoll::Waiting => return Ok(NativePreparation::Original(None)),
            NativeReceiptPoll::Unpublished => {}
        }
        let host = self.host.verify(&context).await?;
        Ok(NativePreparation::Claim(NativeClaim {
            selection,
            context,
            capabilities,
            host,
            admission,
        }))
    }
    pub(in crate::application) async fn process_native_in(
        &self,
        tx: &mut dyn Transaction,
        request: NativeProcessing,
    ) -> Result<ProcessingReady> {
        Admission::check(request.admission.as_ref())?;
        let effect = self
            .claim_processing_in(tx, &request.context, true, request.admission.as_ref())
            .await?;
        Ok(ProcessingReady {
            selection: effect.selection,
            context: request.context,
            capabilities: request.capabilities,
            changed: effect.changed,
            admission: request.admission,
        })
    }
}

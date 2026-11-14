//! Bounded, best-effort runtime diagnostics without application text or secrets.
mod writer;

use agentchat_core::{Error, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::task::JoinHandle;
use uuid::Uuid;

/// Maximum retained files, including the current JSON Lines file.
pub const FILE_COUNT: usize = 3;
/// Each file stays at or below one MiB; rotate before appending a whole record.
pub const FILE_BYTES: usize = 1024 * 1024;
/// Maximum queued fixed-shape records per runtime.
pub const QUEUE_RECORDS: usize = 128;
pub(super) const RECORD_BYTES: usize = 512;

/// Inspect retained offline logs without creating files, repairing tails or
/// exposing contents. A live exclusive writer returns a safe busy observation.
pub async fn inspect(data: impl AsRef<Path>, project: Uuid) -> Result<Value> {
    let data = data.as_ref().to_owned();
    crate::project::blocking(move || writer::inspect(data, project)).await
}

/// Closed diagnostic vocabulary. There is intentionally no arbitrary string,
/// body, command, path, host session ID or error message field.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
    /// One finite RPC's coordinator queue and execution durations.
    RpcTiming {
        /// Public correlation UUID, absent for internal observer page reads.
        request_id: Option<Uuid>,
        /// Time until the coordinator begins processing, in this process.
        queue_us: u64,
        /// Processing through the result, including any shared commit wait.
        execution_us: u64,
    },
    /// A new business event became durable in its FULL mutation commit.
    MutationCommitted {
        /// Original successful mutation retry/correlation key.
        request_id: Uuid,
        /// Durable project event ordering; never a timestamp.
        seq: agentchat_core::sequence::Sequence,
    },
    /// One full-input stage. Diagnostics never settle its delivery outcome.
    NativeInput {
        /// Stable recipient delivery identity.
        delivery_id: Uuid,
        /// Original committed project sequence.
        seq: agentchat_core::sequence::Sequence,
        /// Observed application/port boundary.
        phase: NativePhase,
        /// Poll an original published receipt without emitting new input.
        receipt_only: bool,
        /// Native I/O duration for a result, zero for other phases.
        duration_us: u64,
        /// Port-reported result; only the application can commit acceptance.
        outcome: Option<NativeOutcome>,
    },
    /// The application coordinator is ready to process requests.
    Started,
    /// The serving loop stopped and application cleanup was attempted.
    Stopped,
    /// Application cleanup returned a stable error.
    ShutdownFailed {
        /// Machine-readable classification only.
        code: ErrorCode,
    },
    /// One coordinated call completed successfully.
    RequestSucceeded,
    /// One coordinated call returned a stable application error.
    RequestFailed {
        /// Machine-readable classification only.
        code: ErrorCode,
    },
    /// Lifecycle/lease maintenance failed.
    MaintenanceFailed {
        /// Machine-readable classification only.
        code: ErrorCode,
    },
    /// Native presentation preparation/completion failed.
    NativeFailed {
        /// Machine-readable classification only.
        code: ErrorCode,
    },
    /// The namespace no longer validates against its project reference.
    ProjectInvalid,
    /// The Unix listener stopped accepting connections.
    ListenerFailed,
    /// The accepted-connection budget rejected a new socket.
    ConnectionLimit,
}
/// Closed full-input pipeline points, separate from model/read observations.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativePhase {
    /// Durable head claimed; the final snapshot/host checks still follow.
    DispatchStarted,
    /// The native call returned or its existing deadline elapsed.
    NativeResult,
    /// Exact native result and stream settlement passed the shared FULL commit.
    AcceptanceCommitted,
}
/// Adapter result classification, never independently an acceptance receipt.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeOutcome {
    /// Port reported whole-input native acceptance.
    Accepted,
    /// Only durable local publication is known.
    AwaitingAcceptance,
    /// Positive proof of no native acceptance.
    NotAccepted,
    /// Error, timeout or loss of the result.
    Unknown,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    version: u32,
    project_id: Uuid,
    instance_id: Uuid,
    at_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    runtime_elapsed_us: Option<u64>,
    event: Event,
}
#[derive(Default)]
struct State {
    stopping: AtomicBool,
    finished: AtomicBool,
    failed: AtomicBool,
    dropped: AtomicU64,
}
/// Cloneable nonblocking sink. Saturation drops diagnostics, never business work.
#[derive(Clone)]
pub struct Sink {
    sender: mpsc::SyncSender<Record>,
    state: Arc<State>,
    project: Uuid,
    instance: Uuid,
    started: Instant,
}
impl Sink {
    /// Enqueue a bounded record, returning false when closed, failed or full.
    /// Diagnostics are not durable receipts and must not influence policy.
    pub fn record(&self, event: Event) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let record = Record {
            version: 1,
            project_id: self.project,
            instance_id: self.instance,
            at_unix_ms: u64::try_from(now.as_millis()).unwrap_or(u64::MAX),
            runtime_elapsed_us: Some(elapsed(self.started)),
            event,
        };
        if !self.state.stopping.load(Ordering::Relaxed) && self.sender.try_send(record).is_ok() {
            return true;
        }
        let _ = self
            .state
            .dropped
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                Some(value.saturating_add(1))
            });
        false
    }
    /// Safe live health/budgets; no filesystem reads or diagnostic contents.
    pub fn status(&self) -> agentchat_protocol::health::DiagnosticHealth {
        agentchat_protocol::health::DiagnosticHealth {
            state: if self.state.failed.load(Ordering::Relaxed) {
                "failed"
            } else if self.state.finished.load(Ordering::Relaxed) {
                "closed"
            } else if self.state.stopping.load(Ordering::Relaxed) {
                "closing"
            } else {
                "running"
            }
            .into(),
            dropped_records: self.state.dropped.load(Ordering::Relaxed),
            queue_records: QUEUE_RECORDS,
            record_bytes: RECORD_BYTES,
            file_bytes: FILE_BYTES,
            file_count: FILE_COUNT,
        }
    }
}
/// One independently locked writer. Explicit close drains its bounded queue;
/// dropping the owner also stops the worker even if sinks remain alive.
pub struct RuntimeLog {
    sink: Sink,
    worker: Option<JoinHandle<Result<()>>>,
}
impl RuntimeLog {
    /// Open private project logs, rejecting unsafe files before appending.
    /// A separate writer lock fences a predecessor's final asynchronous cleanup.
    pub async fn open(data: impl AsRef<Path>, project: Uuid, instance: Uuid) -> Result<Self> {
        let path = data.as_ref().to_owned();
        let writer = crate::project::blocking(move || writer::Writer::open(path, project)).await?;
        let (sender, receiver) = mpsc::sync_channel(QUEUE_RECORDS);
        let state = Arc::new(State::default());
        let sink = Sink {
            sender,
            state: state.clone(),
            project,
            instance,
            started: Instant::now(),
        };
        let worker = tokio::task::spawn_blocking(move || {
            let mut writer = writer;
            let result = (|| {
                while !state.stopping.load(Ordering::Relaxed) {
                    match receiver.recv_timeout(Duration::from_millis(50)) {
                        Ok(record) => writer.append(&record)?,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                // Producers reject new work after stopping; at most this many
                // pre-close records are retained. Late racing records may drop.
                for record in receiver.try_iter().take(QUEUE_RECORDS) {
                    writer.append(&record)?;
                }
                writer.finish()
            })();
            state.failed.store(result.is_err(), Ordering::Relaxed);
            state.finished.store(true, Ordering::Relaxed);
            result
        });
        Ok(Self {
            sink,
            worker: Some(worker),
        })
    }
    /// Obtain a nonblocking producer/health view without extending owner lifetime.
    pub fn sink(&self) -> Sink {
        self.sink.clone()
    }
    /// Stop accepting diagnostics, drain queued records and synchronize output.
    /// Records racing with close are best effort, like all routine diagnostics.
    pub async fn close(mut self) -> Result<()> {
        self.sink.state.stopping.store(true, Ordering::Relaxed);
        self.worker
            .take()
            .expect("owned writer")
            .await
            .map_err(|_| Error::new(ErrorCode::IoError, "Runtime diagnostic worker failed."))?
    }
}
pub(crate) fn elapsed(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX)
}
impl Drop for RuntimeLog {
    fn drop(&mut self) {
        self.sink.state.stopping.store(true, Ordering::Relaxed);
    }
}

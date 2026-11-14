//! Metadata-only lifecycle work, independent from native response forwarding.
use super::{
    ProxyDecision, ProxyEvent, ProxyFence,
    metadata::{self, CandidateOutcome},
    observation_guard::Guard,
};
use crate::{
    codex::{required, unavailable},
    launch::{Binding, NativeProof},
};
use agentchat_core::Result;
use futures_util::{FutureExt, future::BoxFuture};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
const TIMEOUT: Duration = Duration::from_secs(35);
#[derive(Clone, PartialEq, Eq)]
struct Key {
    connection: u64,
    epoch: u64,
    id: Option<String>,
}
pub(super) struct Response {
    key: Key,
    outcome: CandidateOutcome,
}
enum Input {
    Request {
        connection: u64,
        request: Option<(String, Option<String>)>,
        ready: oneshot::Sender<Result<()>>,
    },
    Response(Response),
    Closed(u64),
}
#[derive(Clone)]
pub(super) struct Observer {
    sender: mpsc::Sender<Input>,
    slot: Arc<Mutex<Option<Key>>>,
    guard: Arc<Guard>,
}
impl Observer {
    pub(super) fn release_initial(&self, fence: &mut ProxyFence) -> Result<()> {
        self.guard.release_initial(fence)
    }
    pub(super) fn track(&self, binding: &Binding) -> Result<()> {
        self.guard.track(binding)
    }
    pub(super) fn enabled(&self) -> bool {
        self.guard.enabled()
    }
    pub(super) async fn request(&self, connection: u64, message: &Message) -> Result<()> {
        if !self.enabled() {
            return Ok(());
        }
        let request = match metadata::candidate_request(message) {
            Ok(Some(request)) => Some(request),
            Err(_) => None,
            Ok(None) if matches!(message, Message::Binary(_)) => None,
            Ok(None) => return Ok(()),
        };
        let (ready, fenced) = oneshot::channel();
        self.sender
            .send(Input::Request {
                connection,
                request,
                ready,
            })
            .await
            .map_err(|_| unavailable())?;
        fenced.await.map_err(|_| unavailable())?
    }
    pub(super) fn pending(&self, connection: u64) -> bool {
        self.slot
            .lock()
            .expect("correlation poisoned")
            .as_ref()
            .is_some_and(|key| key.connection == connection)
    }
    pub(super) fn response(&self, connection: u64, message: &Message) -> Option<Response> {
        let slot = self.slot.lock().expect("correlation poisoned");
        let key = slot.as_ref().filter(|key| key.connection == connection)?;
        let (_, outcome) =
            metadata::candidate_response(message, |id| key.id.as_deref() == Some(id)).ok()??;
        Some(Response {
            key: key.clone(),
            outcome,
        })
    }
    pub(super) async fn forwarded(&self, response: Response) {
        let _ = self.sender.send(Input::Response(response)).await;
    }
    pub(super) async fn closed(&self, connection: u64) {
        let _ = self.sender.send(Input::Closed(connection)).await;
    }
}
pub(super) fn start(
    home: &Path,
    backend: &Path,
    active_root: &str,
    proof: NativeProof,
    sender: mpsc::Sender<ProxyEvent>,
) -> (Observer, tokio::task::JoinHandle<()>) {
    start_inner(home, backend, active_root, Some(proof), sender)
}
#[cfg(test)]
pub(super) fn unbound(
    active_root: &str,
    sender: mpsc::Sender<ProxyEvent>,
) -> (Observer, tokio::task::JoinHandle<()>) {
    // This transport-only observer cannot track a binding or grant authority.
    start_inner(
        Path::new("/unbound"),
        Path::new("/unbound"),
        active_root,
        None,
        sender,
    )
}
fn start_inner(
    home: &Path,
    backend: &Path,
    active_root: &str,
    proof: Option<NativeProof>,
    sender: mpsc::Sender<ProxyEvent>,
) -> (Observer, tokio::task::JoinHandle<()>) {
    let (input, receiver) = mpsc::channel(16);
    let observer = Observer {
        sender: input,
        slot: Arc::new(Mutex::new(None)),
        guard: Arc::new(Guard::new(home, backend, proof)),
    };
    let mut work = Work {
        sender,
        receiver,
        slot: observer.slot.clone(),
        guard: observer.guard.clone(),
        root: active_root.into(),
        epoch: 0,
        pending: None,
        waiting: None,
        lost: false,
        retained_fence: None,
    };
    let task = tokio::spawn(async move {
        let _ = work.run().await;
    });
    (observer, task)
}
struct Pending {
    key: Key,
    expected: Option<String>,
    outcome: Option<CandidateOutcome>,
    response_seen: bool,
    target: Option<String>,
}
enum Ack {
    Fenced(bool),
    RolledBack(bool),
    Decision(Option<ProxyDecision>),
    Delivered(bool),
}
struct Work {
    sender: mpsc::Sender<ProxyEvent>,
    receiver: mpsc::Receiver<Input>,
    slot: Arc<Mutex<Option<Key>>>,
    guard: Arc<Guard>,
    root: String,
    epoch: u64,
    pending: Option<Pending>,
    waiting: Option<BoxFuture<'static, Ack>>,
    lost: bool,
    retained_fence: Option<oneshot::Receiver<()>>,
}
impl Work {
    async fn run(&mut self) -> Result<()> {
        loop {
            tokio::select! {
                biased;
                ack = async { self.waiting.as_mut().expect("guarded wait").await }, if self.waiting.is_some() => {
                    self.waiting = None;
                    match ack {
                        Ack::Fenced(true) => {
                            if self.pending.as_ref().is_some_and(|p| p.key.id.is_none()) { self.unknown().await; }
                            else { self.outcome().await?; }
                        }
                        Ack::RolledBack(true) | Ack::Delivered(true) => {
                            let key = &self.pending.as_ref().expect("pending acknowledgement").key;
                            self.guard.release(key.connection, key.epoch);
                            self.clear();
                        }
                        Ack::Decision(Some(_)) => {
                            let pending = self.pending.as_ref().expect("pending decision");
                            self.root = pending.target.clone().expect("success root");
                            let (release, released) = oneshot::channel();
                            self.sender.send(ProxyEvent::Delivered {
                                connection_epoch: pending.key.connection, transition_epoch: pending.key.epoch, release,
                            }).await.map_err(|_| unavailable())?;
                            self.waiting = Some(async move { Ack::Delivered(acknowledged(released).await) }.boxed());
                        }
                        _ => self.unknown().await,
                    }
                },
                input = self.receiver.recv() => match input {
                    Some(Input::Request { connection, request, ready }) => {
                        let result = self.request(connection, request).await;
                        let _ = ready.send(result);
                    }
                    Some(Input::Response(response)) => {
                        if let Some(pending) = self.pending.as_mut()
                            && pending.key == response.key && !pending.response_seen && pending.outcome.is_none() {
                            pending.outcome = Some(response.outcome);
                            if self.waiting.is_none() { self.outcome().await?; }
                        }
                    }
                    Some(Input::Closed(connection)) => {
                        if self.pending.as_ref().is_some_and(|p| p.key.connection == connection
                            && !p.response_seen && p.outcome.is_none()) { self.unknown().await; }
                    }
                    None => return Ok(()),
                }

            }
        }
    }
    async fn request(
        &mut self,
        connection: u64,
        request: Option<(String, Option<String>)>,
    ) -> Result<()> {
        if self.lost {
            return Ok(());
        }
        if let Some(pending) = &self.pending {
            // Overlap has no unambiguous foreground result. Re-fence the current
            // tracked binding (which activation may already have replaced), then
            // quarantine observation without rejecting either native request.
            let key = pending.key.clone();
            let direct = self.guard.fence(key.connection, key.epoch)?;
            let (fence, fenced) = oneshot::channel();
            self.sender
                .send(ProxyEvent::Candidate {
                    connection_epoch: key.connection,
                    transition_epoch: key.epoch,
                    fence: ProxyFence::new(fence),
                })
                .await
                .map_err(|_| unavailable())?;
            if !direct {
                if !acknowledged(fenced).await {
                    return Err(unavailable());
                }
            } else {
                self.retained_fence = Some(fenced);
            }
            // Retain the receiver until the runtime has acknowledged this
            // duplicate fence; cancellation must never affect native traffic.
            self.unknown().await;
            return Ok(());
        }
        self.epoch = self.epoch.checked_add(1).ok_or_else(required)?;
        let (id, expected) = request.map_or((None, None), |(id, root)| (Some(id), root));
        let key = Key {
            connection,
            epoch: self.epoch,
            id,
        };
        let direct = self.guard.fence(key.connection, key.epoch)?;
        let (fence, fenced) = oneshot::channel();
        self.sender
            .send(ProxyEvent::Candidate {
                connection_epoch: key.connection,
                transition_epoch: key.epoch,
                fence: ProxyFence::new(fence),
            })
            .await
            .map_err(|_| unavailable())?;
        *self.slot.lock().expect("correlation poisoned") = Some(key.clone());
        self.pending = Some(Pending {
            key,
            expected,
            outcome: None,
            response_seen: false,
            target: None,
        });
        if direct {
            self.waiting = Some(async move { Ack::Fenced(acknowledged(fenced).await) }.boxed());
        } else {
            // Before initial readiness there is no tracked binding. The original
            // supervisor must confirm the fence before this request can enter.
            if !acknowledged(fenced).await {
                self.unknown().await;
                return Err(unavailable());
            }
            if self.pending.as_ref().is_some_and(|p| p.key.id.is_none()) {
                self.unknown().await;
            }
        }
        Ok(())
    }
    async fn outcome(&mut self) -> Result<()> {
        let Some(pending) = self.pending.as_mut() else {
            return Ok(());
        };
        let Some(outcome) = pending.outcome.take() else {
            return Ok(());
        };
        pending.response_seen = true;
        let key = &pending.key;
        match outcome {
            CandidateOutcome::Succeeded(root)
                if pending
                    .expected
                    .as_deref()
                    .is_some_and(|expected| expected != root) =>
            {
                self.unknown().await
            }
            CandidateOutcome::Succeeded(root) if root != self.root => {
                pending.target = Some(root.clone());
                let (release, decision) = oneshot::channel();
                self.sender
                    .send(ProxyEvent::Succeeded {
                        connection_epoch: key.connection,
                        transition_epoch: key.epoch,
                        root,
                        release,
                    })
                    .await
                    .map_err(|_| unavailable())?;
                self.waiting = Some(
                    async move {
                        Ack::Decision(
                            tokio::time::timeout(TIMEOUT, decision)
                                .await
                                .ok()
                                .and_then(|r| r.ok()),
                        )
                    }
                    .boxed(),
                );
            }
            CandidateOutcome::Failed | CandidateOutcome::Succeeded(_) => {
                let (release, released) = oneshot::channel();
                self.sender
                    .send(ProxyEvent::RolledBack {
                        connection_epoch: key.connection,
                        transition_epoch: key.epoch,
                        release,
                    })
                    .await
                    .map_err(|_| unavailable())?;
                self.waiting =
                    Some(async move { Ack::RolledBack(acknowledged(released).await) }.boxed());
            }
            CandidateOutcome::Indeterminate => self.unknown().await,
        }
        Ok(())
    }
    async fn unknown(&mut self) {
        if let Some(pending) = &self.pending {
            let _ = self
                .sender
                .send(ProxyEvent::Unknown {
                    connection_epoch: pending.key.connection,
                    transition_epoch: pending.key.epoch,
                })
                .await;
        }
        self.lost = true;
        self.guard.disable();
        self.clear();
    }
    fn clear(&mut self) {
        self.pending = None;
        self.waiting = None;
        *self.slot.lock().expect("correlation poisoned") = None;
    }
}
async fn acknowledged(receiver: oneshot::Receiver<()>) -> bool {
    matches!(tokio::time::timeout(TIMEOUT, receiver).await, Ok(Ok(())))
}

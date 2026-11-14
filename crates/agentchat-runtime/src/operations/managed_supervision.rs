//! One sequential application future beside native exit and local control.
use super::{Operations, Route, activation::Control};
use crate::launch::LaunchEvents;
use agentchat_core::{Result, model::LaunchOwnerState};
use agentchat_hosts::launch::NativeChild;
use std::{future::Future, os::unix::process::ExitStatusExt, pin::Pin, time::Duration};
use tokio::signal::unix::{Signal, SignalKind, signal};

type Work<'a> = Pin<Box<dyn Future<Output = Result<Control>> + Send + 'a>>;
impl Operations {
    pub(super) async fn supervise_ready<'a>(
        &'a self,
        route: &'a Route,
        mut child: NativeChild,
        startup: Work<'a>,
        events: &'a dyn LaunchEvents,
        mut signals: (Signal, Signal),
    ) -> Result<i32> {
        let proof = child.proof();
        let mut control: Option<Control> = None;
        let mut pending = Some(startup);
        let mut initialized = false;
        let mut listening = true;
        let mut reported = None;
        // Native callbacks commit lifecycle state before returning. A bounded
        // local poll lets the supervisor consume that state only after callback
        // locks/getters are released, avoiding a callback/getter deadlock while
        // keeping fresh-root restoration inside its finite startup window.
        let period = Duration::from_millis(250);
        let mut ticks = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let status = loop {
            tokio::select! {
                result=child.wait()=>break result?,
                _=signals.0.recv()=>{},
                _=signals.1.recv()=>break child.terminate().await?,
                result=async {pending.as_mut().expect("guarded work").as_mut().await},if pending.is_some()=>{
                    pending=None;
                    match result {
                        Ok(value)=>{
                            if !initialized { events.authorized(&child,value.authorization())?; initialized=true; }
                            if let Some(started)=&value.started {
                                let key=(started.identity.generation,started.context.context_id);
                                if reported!=Some(key) {
                                    if reported.is_some_and(|(generation,_)|generation==key.0) {events.reattached(started)?;}
                                    else {events.activated(started)?;}
                                    reported=Some(key);
                                }
                            }
                            control=Some(value);
                        },
                        Err(error)=>{
                            let _=events.connection_lost(&error);
                            let status=child.terminate().await?;
                            let _=events.exited(&child,status);
                            return Err(error);
                        },
                    }
                },
                stream=async {control.as_ref().expect("guarded control").accept().await},if listening&&control.is_some()&&pending.is_none()=>{
                    let stream=match stream {
                        Ok(stream)=>stream,
                        Err(error)=>{
                            // Losing the activation endpoint cannot force an
                            // active native turn to exit. Preserve its physical
                            // child and suspend new control accepts; current
                            // membership/owner policy remains independently gated.
                            events.connection_lost(&error)?;
                            listening=false;
                            continue;
                        },
                    };
                    let mut owned=control.take().expect("accepted control");
                    let proof=proof.clone();
                    pending=Some(Box::pin(async move {owned.serve(self,route,&proof,stream).await;Ok(owned)}));
                },
                _=ticks.tick(),if control.is_some()&&pending.is_none()=>{
                    let mut owned=control.take().expect("guarded control");
                    let proof=proof.clone();
                    pending=Some(Box::pin(async move {
                        if proof.state()==LaunchOwnerState::Current
                            && let Err(error)=owned.tick(self,route,&proof).await {
                            // Observation/activation faults suspend AgentChat;
                            // the native transport and terminal remain owned.
                            events.connection_lost(&error)?;
                        }
                        Ok(owned)
                    }));
                },
            }
        };
        // Cancelling a control exchange after actual native exit cannot renew
        // its authority. Maintenance retires its last known committed generation.
        drop(pending);
        if let Some(owned) = control.as_mut() {
            owned.reconcile(self, route).await;
        }
        events.exited(&child, status)?;
        Ok(status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)))
    }
}
pub(super) fn signals() -> Result<(Signal, Signal)> {
    Ok((
        signal(SignalKind::interrupt()).map_err(|_| super::activation::signal_error())?,
        signal(SignalKind::terminate()).map_err(|_| super::activation::signal_error())?,
    ))
}

#[cfg(test)]
#[path = "../../tests/unit/managed_supervision.rs"]
mod tests;

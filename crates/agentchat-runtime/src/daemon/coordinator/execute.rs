//! Coordinator application dispatch with typed read composition.
use super::*;
use agentchat_core::{model::OperationActor, read::requests};
use agentchat_protocol::rpc::ActorReference;
use serde_json::json;

impl Worker {
    pub(super) async fn execute(
        &mut self,
        connection: Uuid,
        call: Call,
        bound: Option<Uuid>,
    ) -> Result<Answer> {
        let context = self.resolve_context(&call, bound).await?;
        let mut bind = false;
        let subscription = None;
        let result = match call {
            Call::Observe => {
                context_free(&context)?;
                let hints = self.heads.subscribe();
                let head = self.app.read(None, requests::ViewerHead).await?;
                return Ok(Answer {
                    result: Response::ObserverStart(Box::new(
                        agentchat_protocol::rpc::ObserverStart { head },
                    )),
                    subscription: Some(Subscription {
                        kind: FollowKind::Hold,
                        hints,
                    }),
                    bind: false,
                });
            }
            Call::SubscribeChanges { after } => {
                context_free(&context)?;
                // Register before validating/scanning the retained boundary.
                // The watch receiver coalesces hints; it stores no event bodies.
                let hints = self.heads.subscribe();
                self.app
                    .read(
                        None,
                        requests::ViewerChanges {
                            changes: agentchat_core::read::ChangeRequest {
                                after,
                                through: Some(after),
                                limit: Some(1),
                            },
                        },
                    )
                    .await?;
                let head = self.app.read(None, requests::ViewerHead).await?;
                return Ok(Answer {
                    result: Response::ChangeStart(Box::new(agentchat_protocol::rpc::ChangeStart {
                        after,
                        head,
                    })),
                    subscription: Some(Subscription {
                        kind: FollowKind::Changes { after },
                        hints,
                    }),
                    bind: false,
                });
            }
            Call::StartLaunch { name, .. } => {
                let context = context.as_ref().ok_or_else(session)?;
                let committed = self.app.start_launch(context, name.as_deref()).await?;
                self.pending.remove(&context.context_id);
                self.attached.insert(connection, context.clone());
                if let Some(head) = committed.event_head {
                    self.heads.send_replace(head.cursor());
                }
                return Ok(Answer {
                    result: committed.result.into(),
                    subscription: None,
                    bind: true,
                });
            }
            Call::ActivateManaged {
                request_id,
                cause,
                name,
                ..
            } => {
                let context = context.as_ref().ok_or_else(session)?;
                let activated = self
                    .app
                    .activate_managed_request(
                        agentchat_core::application::ManagedActivationRequest {
                            request_id,
                            target_context: context.clone(),
                            cause,
                            name,
                        },
                    )
                    .await?;
                self.pending.remove(&context.context_id);
                self.heads.send_replace(activated.join_boundary.cursor());
                return Ok(Answer {
                    result: serde_json::to_value(activated)
                        .map_err(|_| {
                            Error::new(
                                ErrorCode::IntegrityError,
                                "Managed activation result encoding failed.",
                            )
                        })?
                        .into(),
                    subscription: None,
                    bind: false,
                });
            }
            Call::ReconcileLaunch { generation, .. } => {
                let committed = self.app.reconcile_launch(generation).await?;
                if let Some(head) = committed.event_head {
                    self.heads.send_replace(head.cursor());
                }
                self.reconcile_delivery_contexts().await;
                committed.result
            }
            Call::Mutate {
                request_id,
                command,
                actor,
            } => {
                if matches!(actor, ActorReference::User {}) {
                    let committed = self
                        .app
                        .execute_actor(request_id, &OperationActor::User, command)
                        .await?;
                    return Ok(self.publish_user_mutation(request_id, committed));
                }
                let context = context.as_ref().ok_or_else(session)?;
                let leaving = matches!(&command, agentchat_core::application::Mutation::Leave);
                let committed = self.app.execute(request_id, context, command).await?;
                return Ok(self.publish_mutation(request_id, context, leaving, committed));
            }
            Call::Query { query, actor } => {
                let subject = if matches!(actor, Some(ActorReference::User {})) {
                    Some(OperationActor::User)
                } else {
                    context.clone().map(OperationActor::Native)
                };
                let result = self.app.query_actor(subject.as_ref(), query).await?;
                let result =
                    result.map_whoami(|application| agentchat_protocol::health::RuntimeWhoami {
                        application,
                        runtime: self.runtime_health(),
                    });
                return Ok(Answer {
                    result: Response::Read(Box::new(result)),
                    subscription: None,
                    bind: false,
                });
            }
            Call::NativeBinding { .. } => {
                return Ok(Answer {
                    result: Response::NativeBinding(Box::new(
                        self.app
                            .native_binding(context.as_ref().ok_or_else(session)?)
                            .await?,
                    )),
                    subscription: None,
                    bind: false,
                });
            }
            Call::InternalHealth => {
                return Ok(Answer {
                    result: Response::InternalHealth(Box::new(
                        agentchat_protocol::health::InternalHealth {
                            application: self.app.health().await?,
                            runtime: self.runtime_health(),
                        },
                    )),
                    subscription: None,
                    bind: false,
                });
            }
            Call::CheckDelivery { .. } | Call::CheckMessage { .. } | Call::CheckNative { .. } => {
                return Err(Error::new(
                    ErrorCode::UnsupportedProtocol,
                    "Native checks require the independent snapshot path.",
                ));
            }
            Call::Attach { .. } => {
                let context = context.as_ref().ok_or_else(session)?;
                let agent = self.app.attach(context).await?;
                if agent.as_ref().is_some_and(|a| a.joined) {
                    // A replacement transport can restore this still-current
                    // incarnation before deferred last-connection cleanup runs.
                    self.pending.remove(&context.context_id);
                    self.attached.insert(connection, context.clone());
                    bind = true;
                }
                json!({"agent":agent})
            }
            Call::Heartbeat { .. } => {
                self.app
                    .heartbeat(context.as_ref().ok_or_else(session)?)
                    .await?;
                json!({})
            }
            Call::Detach { .. } => {
                let context = context.as_ref().ok_or_else(session)?;
                self.app.detach(context).await?;
                self.deliveries.cancel(context.context_id);
                self.attached
                    .retain(|_, c| c.context_id != context.context_id);
                json!({})
            }
            Call::Handshake(_) => {
                return Err(Error::new(
                    ErrorCode::UnsupportedProtocol,
                    "Handshake is already complete.",
                ));
            }
        };
        Ok(Answer {
            result: result.into(),
            subscription,
            bind,
        })
    }

    fn runtime_health(&self) -> agentchat_protocol::health::RuntimeHealth {
        agentchat_protocol::health::RuntimeHealth {
            activity: self.activity.status(),
            instance_id: self.instance,
            last_control_error: self.last_error,
            diagnostics: self.diagnostics.status(),
            native_delivery: self.deliveries.status(),
            mutation_groups: self.grouping.status(),
        }
    }
}

fn context_free(context: &Option<Context>) -> Result<()> {
    if context.is_some() {
        return Err(Error::new(
            ErrorCode::IdentityConflict,
            "Viewer observation requires an unbound context-free connection.",
        ));
    }
    Ok(())
}

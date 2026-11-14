//! One owned typed source, with at most one complete raw continuation retained.

use super::{Fields, Types, input};
use crate::{projection::Changes, render::Renderer};
use agentchat_core::{Error, Result, read::ChangeBatch};
use agentchat_protocol::viewer::ChangeObserver;
use agentchat_viewer_contract as contract;
use chigiri::{FieldContext, SubscriptionOverflow, SubscriptionSource};
use std::sync::Arc;

struct Source {
    observer: Box<dyn ChangeObserver>,
    renderer: Renderer,
    pending: Option<ChangeBatch>,
    failed: bool,
}
impl contract::SubscriptionResolver<bool, Types, Error> for Fields {
    type ChangesEvent = Arc<Changes>;
    async fn subscribe_changes<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        args: &'a contract::SubscriptionChangesArgs,
    ) -> Result<SubscriptionSource<Arc<Changes>, Error>> {
        let observer = self.reader.watch(input::checkpoint(&args.after)?).await?;
        let source = Source {
            observer,
            renderer: self.renderer.clone(),
            pending: None,
            failed: false,
        };
        // No producer task/queue, polling timer or borrowed setup state. The SDK
        // demands one event at a time under its lossless backpressure policy.
        let stream = futures_util::stream::unfold(source, |mut source| async move {
            if source.failed {
                return None;
            }
            let next = match source.pending.take() {
                Some(batch) => Ok(batch),
                None => source.observer.next().await,
            };
            let result = match next {
                Ok(batch) => match Changes::prepare(batch, &source.renderer).await {
                    Ok((batch, rest)) => {
                        source.pending = rest;
                        Ok(batch)
                    }
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            };
            source.failed = result.is_err();
            Some((result, source))
        });
        Ok(SubscriptionSource::new(stream).with_overflow(SubscriptionOverflow::Backpressure))
    }
    async fn resolve_changes_event<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        _: &'a contract::SubscriptionChangesArgs,
        event: &'a Arc<Changes>,
    ) -> Result<Arc<Changes>> {
        Ok(event.clone())
    }
}

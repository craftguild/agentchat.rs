//! Independent public RPC clients, explicit receiving acknowledgements and reads.
use super::capture::{Capture, Input};
use agentchat_core::{
    application::Mutation,
    model::Context,
    read::{InboxView, Page, PageRequest, Query},
    sequence::Cursor,
};
use agentchat_protocol::rpc::{Call, Client, success};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch},
    time::{Instant, MissedTickBehavior},
};
use uuid::Uuid;
static TIMINGS: OnceLock<Mutex<BTreeMap<&'static str, Vec<f64>>>> = OnceLock::new();
fn timed(kind: &'static str, start: Instant) {
    let mut all = TIMINGS.get_or_init(Default::default).lock().unwrap();
    let values = all.entry(kind).or_default();
    assert!(values.len() < 100_000);
    values.push(start.elapsed().as_secs_f64() * 1000.0);
}
pub fn timings() -> Value {
    let all = TIMINGS.get_or_init(Default::default).lock().unwrap();
    let mut result = serde_json::Map::new();
    for (kind, values) in all.iter() {
        let mut sorted = values.clone();
        sorted.sort_by(f64::total_cmp);
        result.insert(
            (*kind).into(),
            serde_json::json!({"count":sorted.len(),"p50_ms":sorted[sorted.len()/2],
            "p95_ms":sorted[(sorted.len()*95).div_ceil(100)-1],"max_ms":sorted.last()}),
        );
    }
    Value::Object(result)
}

pub async fn mutate(client: &mut Client, context: &Context, command: Mutation, key: Uuid) -> Value {
    let kind = match &command {
        Mutation::Post { .. } => "post",
        Mutation::Read { .. } => "read",
        Mutation::ChannelCreate { .. } => "channel",
        Mutation::Reserve { .. } => "reserve",
        _ => "control",
    };
    let start = Instant::now();
    let result = success(
        client
            .call(
                key,
                Call::Mutate {
                    request_id: key,
                    actor: agentchat_protocol::rpc::ActorReference::Native {
                        context_id: context.context_id,
                    },
                    command,
                },
            )
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "workload transport failed: kind={kind} request={key} context={} elapsed={:?} error={error:?}",
                    context.context_id,
                    start.elapsed()
                )
            }),
    )
    .unwrap_or_else(|error| {
        panic!(
            "workload mutation failed: kind={kind} request={key} context={} elapsed={:?} error={error:?}",
            context.context_id,
            start.elapsed()
        )
    });
    timed(kind, start);
    result
}
pub async fn health(client: &mut Client) -> Value {
    let start = Instant::now();
    let result = serde_json::to_value(client.health().await.unwrap()).unwrap();
    timed("health", start);
    result
}
pub async fn query(client: &mut Client, context: Option<Uuid>, query: Query) -> Value {
    let kind = if matches!(&query, Query::Inbox { .. }) {
        "inbox"
    } else {
        "status"
    };
    let start = Instant::now();
    let result = serde_json::to_value(
        success(client.query(Uuid::new_v4(), context, query).await.unwrap()).unwrap(),
    )
    .unwrap();
    timed(kind, start);
    result
}
pub async fn writer(
    mut client: Client,
    context: Context,
    index: usize,
    count: usize,
    start: Instant,
    capture: Arc<Capture>,
) {
    let mut tick = tokio::time::interval_at(
        start + Duration::from_millis(index as u64 * 100),
        Duration::from_millis(500),
    );
    tick.set_missed_tick_behavior(MissedTickBehavior::Burst);
    for number in 0..count {
        tick.tick().await;
        let key = Uuid::new_v4();
        capture.track(key);
        let result = mutate(
            &mut client,
            &context,
            Mutation::Post {
                channel: Some("dev".into()),
                body: format!(
                    "@load{} workload message actor={index} number={number}",
                    (index + 1) % 5
                ),
            },
            key,
        )
        .await;
        assert_eq!(result["mentioned_agent_ids"].as_array().unwrap().len(), 1);
        assert!(result["warnings"].as_array().unwrap().is_empty());
    }
}
pub async fn side(
    mut client: Client,
    context: Context,
    index: usize,
    cycles: usize,
    period: Duration,
    start: Instant,
) {
    let offset = period.mul_f64(index as f64 / 5.0) + Duration::from_millis(100);
    let mut tick = tokio::time::interval_at(start + offset, period);
    tick.set_missed_tick_behavior(MissedTickBehavior::Burst);
    for number in 0..cycles {
        tick.tick().await;
        let created = mutate(
            &mut client,
            &context,
            Mutation::ChannelCreate {
                name: format!("load-{index}-{number}"),
            },
            Uuid::new_v4(),
        )
        .await;
        assert_eq!(created["membership_count"], 0);
        let group = mutate(
            &mut client,
            &context,
            Mutation::Reserve {
                patterns: vec![format!("load/{index}/**")],
                ttl_seconds: Some(30),
                reason: Some("synthetic workload".into()),
            },
            Uuid::new_v4(),
        )
        .await;
        let id = serde_json::from_value(group["reservation_id"].clone()).unwrap();
        let generation = group["generation"].as_i64().unwrap();
        mutate(
            &mut client,
            &context,
            Mutation::ReservationsRenew {
                reservation_id: id,
                generation,
            },
            Uuid::new_v4(),
        )
        .await;
        mutate(
            &mut client,
            &context,
            Mutation::ReservationsRelease {
                reservation_id: id,
                generation,
            },
            Uuid::new_v4(),
        )
        .await;
    }
}
pub async fn anchor(mut client: Client, context: Context, mut stop: watch::Receiver<bool>) {
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            _=stop.changed()=>{if *stop.borrow(){break;}}
            _=tick.tick()=>{success(client.call(Uuid::new_v4(),Call::Heartbeat {context_id:context.context_id}).await.unwrap()).unwrap();}
        }
    }
}
pub(super) async fn drain(
    client: &mut Client,
    context: &Context,
    agent: Uuid,
    read: &mut HashSet<Uuid>,
    presented: &HashSet<Uuid>,
    app: &(impl crate::support::runtime::NativeInputService + ?Sized),
    progress: &super::drain_progress::ReceiverSlot,
) {
    // One processing boundary consumes at most one finite page. Draining until
    // globally idle would starve later native hints under continuous traffic.
    {
        progress.stage("inbox_query");
        let value = query(
            client,
            Some(context.context_id),
            Query::Inbox {
                page: PageRequest {
                    limit: Some(200),
                    after: Cursor::START,
                },
            },
        )
        .await;
        assert!(serde_json::to_vec(&value).unwrap().len() <= agentchat_core::read::MAX_PAGE_BYTES);
        let page: Page<InboxView> = serde_json::from_value(value).unwrap();
        assert!(page.items.len() <= 200);
        if page.items.is_empty() {
            return;
        }
        let mut ids = Vec::new();
        let mut messages = Vec::new();
        for item in page.items {
            // Preserve FIFO and hint timing samples: do not consume a targeted
            // input or any successor until its hint has been presented.
            if item.delivery.wake_eligible && !presented.contains(&item.delivery.id) {
                break;
            }
            ids.push(item.delivery.id);
            if item.dispatch.kind == agentchat_core::model::DispatchKind::Message {
                let message = item
                    .message
                    .expect("full body is supplied by ordinary inbox");
                assert!(message.message.body.contains(" workload message actor="));
                if message.message.author_id != agent && !read.contains(&message.message.id) {
                    messages.push(message.message.id);
                }
            }
        }
        if ids.is_empty() {
            return;
        }
        // The synthetic recipient receives each original full input through
        // Core before recording independent explicit reads of queried bodies.
        crate::support::runtime::accept_native_observed(app, context, ids, |event| {
            use crate::support::runtime::NativeObservation;
            match event {
                NativeObservation::Preparing => progress.stage("native_prepare"),
                NativeObservation::Prepared(dispatch) => progress.prepared(dispatch),
                NativeObservation::Committed(dispatch) => progress.native(dispatch),
            }
        })
        .await;
        if !messages.is_empty() {
            progress.stage("explicit_read_rpc");
            mutate(
                client,
                context,
                Mutation::Read {
                    message_ids: messages.clone(),
                },
                Uuid::new_v4(),
            )
            .await;
            read.extend(messages);
            progress.read(read.len());
        }
    }
}
pub async fn consumer<S: crate::support::runtime::NativeInputService + ?Sized + 'static>(
    mut client: Client,
    context: Context,
    agent: Uuid,
    mut receive: mpsc::Receiver<Input>,
    app: Arc<S>,
    progress: Arc<super::drain_progress::ReceiverSlot>,
) -> usize {
    let mut read = HashSet::new();
    let mut presented = HashSet::new();
    loop {
        progress.stage("receiving");
        let Some(input) = receive.recv().await else {
            break;
        };
        match input {
            Input::Native(dispatch) => {
                assert_eq!(dispatch.hint.agent_id, agent);
                presented.extend(dispatch.hint.events.iter().map(|e| e.delivery_id));
                drain(
                    &mut client,
                    &context,
                    agent,
                    &mut read,
                    &presented,
                    app.as_ref(),
                    &progress,
                )
                .await;
            }
            Input::Flush(reply) => {
                drain(
                    &mut client,
                    &context,
                    agent,
                    &mut read,
                    &presented,
                    app.as_ref(),
                    &progress,
                )
                .await;
                // This boundary follows the durable explicit Read call; it
                // creates no positive event or additional native obligation.
                let _ = reply.send(read.len());
            }
            Input::Stop => break,
        }
    }
    read.len()
}

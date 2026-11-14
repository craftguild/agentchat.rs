//! Synthetic receiving models read complete native bodies without ACK or fetch.
use super::capture::{Capture, Input};
use agentchat_core::{application::Mutation, model::Context};
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
            .unwrap(),
    )
    .unwrap();
    timed(kind, start);
    result
}
pub async fn health(client: &mut Client) -> Value {
    let start = Instant::now();
    let result = serde_json::to_value(client.health().await.unwrap()).unwrap();
    timed("health", start);
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
pub async fn consumer(
    mut client: Client,
    context: Context,
    agent: Uuid,
    mut receive: mpsc::Receiver<Input>,
) -> usize {
    let mut read = HashSet::new();
    while let Some(input) = receive.recv().await {
        match input {
            Input::Body(input) => {
                assert_ne!(input.author.actor_id, agent);
                assert_eq!(input.author.kind, agentchat_core::model::ActorKind::Agent);
                assert!(input.auto_read);
                assert_eq!(input.channel, "dev");
                let author: usize = input
                    .author
                    .handle
                    .strip_prefix("load")
                    .unwrap()
                    .parse()
                    .unwrap();
                let number: usize = input
                    .body
                    .rsplit("number=")
                    .next()
                    .unwrap()
                    .parse()
                    .unwrap();
                assert_eq!(
                    input.body,
                    format!(
                        "@load{} workload message actor={author} number={number}",
                        (author + 1) % 5
                    )
                );
                assert!(
                    read.insert(input.message_id),
                    "duplicate complete native body"
                );
                // This declaration models explicit reading only. Native
                // acceptance and its automatic ACK do not wait for it.
                mutate(
                    &mut client,
                    &context,
                    Mutation::Read {
                        message_ids: vec![input.message_id],
                    },
                    Uuid::new_v4(),
                )
                .await;
            }
            Input::Flush(reply) => {
                let _ = reply.send(read.len());
            }
            Input::Stop => break,
        }
    }
    read.len()
}

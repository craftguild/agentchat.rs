//! Independent event-stream fan-out accounting and retained snapshot comparison.
use agentchat_core::{
    model::{DeliveryState, Event, EventKind},
    ports::Storage,
    read::{MAX_PAGE_BYTES, MessageFilter, PageRequest, requests},
    sequence::Cursor,
    time::Timestamp,
};
use agentchat_protocol::rpc::{ChangeFollower, Client, success};
use agentchat_store::Store;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
};
use tokio::sync::watch;
use uuid::Uuid;

pub struct Observed {
    pub head: i64,
    pub events: u64,
    pub deliveries: u64,
    pub kinds: HashMap<String, usize>,
    pub digest: String,
    pub timeout_messages: HashSet<Uuid>,
    pub timeout_targets: HashMap<Uuid, Uuid>,
    timeout_facts: HashMap<Uuid, TimeoutFact>,
}
struct TimeoutFact {
    author: Uuid,
    count: u64,
    recipients: Vec<(agentchat_core::read::IdentityLabel, Timestamp)>,
}
fn hash(hash: &mut Sha256, event: &Event) {
    hash.update(serde_json::to_vec(event).unwrap());
    hash.update(b"\n");
}
fn finish(hash: Sha256) -> String {
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
pub async fn observe(
    mut follower: ChangeFollower,
    mut client: Client,
    progress: Arc<AtomicI64>,
    mut stop: watch::Receiver<bool>,
) -> Observed {
    let mut head = 0;
    let mut event_count = 0;
    let mut deliveries = 0;
    let mut kinds = HashMap::new();
    let mut joined = HashSet::new();
    let mut channel_members = HashMap::<Uuid, HashSet<Uuid>>::new();
    let mut digest = Sha256::new();
    let mut timeout_messages = HashSet::new();
    let mut timeout_targets = HashMap::new();
    let mut timeout_facts = HashMap::new();
    loop {
        tokio::select! {
            changed=stop.changed()=>{if changed.is_err()||*stop.borrow(){break;}}
            scan=follower.next()=>{
                let scan=scan.unwrap();
                // The dedicated Viewer feed supplies committed scan progress.
                // Complete audit envelopes still use finite context-free reads;
                // no removed generic subscription or receipt poll is restored.
                while head < scan.scan.seq.get() {
                let page=success(client.read(Uuid::new_v4(),None,requests::Events {
                    channel:None,page:PageRequest {after:Cursor::new(head).unwrap(),limit:Some(200)}
                }).await.unwrap()).unwrap();
                assert!(page.items.len()<=200 && !page.items.is_empty());
                assert!(serde_json::to_vec(&page).unwrap().len()<=MAX_PAGE_BYTES);
                for event in page.items {
                    assert!(event.seq.get()>head);head=event.seq.get();event_count+=1;
                    if event.kind==EventKind::AgentJoined {assert!(joined.insert(event.actor_id.unwrap()));}
                    assert_ne!(event.kind,EventKind::AgentLeft);
                    // Independent reads produce no audit kind in this fresh format.
                    if event.kind==EventKind::MessageDeliveryConfirmationTimeout {
                        assert!(timeout_messages.insert(event.message_id.unwrap()));
                        let author:Uuid=serde_json::from_value(event.data["author_id"].clone()).unwrap();
                        assert!(joined.contains(&author));
                        assert!(event.actor_id.is_none());
                        let count=event.data["expired_count"].as_u64().unwrap();
                        assert!(count>0);
                        let sample=event.data["sample_recipients"].as_array().unwrap();
                        assert!(sample.len()<=20);
                        let recipients=sample.iter().map(|value| {
                            let deadline: Timestamp = serde_json::from_value(value["deadline_at"].clone()).unwrap();
                            let timed_out: Timestamp = serde_json::from_value(value["timed_out_at"].clone()).unwrap();
                            assert!(event.occurred_at > deadline && timed_out <= event.occurred_at);
                            (serde_json::from_value(value["recipient"].clone()).unwrap(), deadline)
                        }).collect();
                        assert!(timeout_targets.insert(event.message_id.unwrap(),author).is_none());
                        assert!(timeout_facts.insert(event.message_id.unwrap(),TimeoutFact {author,count,recipients}).is_none());
                        deliveries+=1;
                    } else if let Some(channel_id)=event.channel_id {
                        let members=channel_members.entry(channel_id).or_default();
                        if event.kind==EventKind::ChannelJoined {assert!(members.insert(event.actor_id.unwrap()));}
                        if event.kind==EventKind::ChannelLeft {assert!(members.remove(&event.actor_id.unwrap()));}
                        deliveries+=members.iter().filter(|agent|Some(**agent)!=event.actor_id).count() as u64;
                    } else {
                        // Explicitly project-global events retain all joined
                        // recipients except their own actor.
                        deliveries+=joined.iter().filter(|agent|Some(**agent)!=event.actor_id).count() as u64;
                    }
                    *kinds.entry(event.kind.as_str().to_owned()).or_default()+=1;
                    hash(&mut digest,&event);
                }
                }
                progress.store(head,Ordering::SeqCst);
            }
        }
    }
    Observed {
        head,
        events: event_count,
        deliveries,
        kinds,
        digest: finish(digest),
        timeout_messages,
        timeout_targets,
        timeout_facts,
    }
}
pub async fn retained(
    store: &Store,
    observed: &Observed,
    messages: usize,
    channels: usize,
    automatic_reads: bool,
) -> Value {
    store.integrity_check().await.unwrap();
    let mut snapshot = store.snapshot().await.unwrap();
    let stats = snapshot.statistics().await.unwrap();
    assert_eq!(stats.agents, 5);
    assert_eq!(stats.joined, 5);
    assert_eq!(stats.messages, messages as u64);
    assert_eq!(stats.channels, channels as u64);
    assert_eq!(stats.events, observed.events);
    assert_eq!(stats.deliveries, observed.deliveries);
    assert_eq!(stats.pending_deliveries, 0);
    assert_eq!(
        stats.read_receipts,
        (if automatic_reads { 8 } else { 4 }) * messages as u64
    );
    assert_eq!(stats.stalled_batches, 0);
    assert_eq!(
        observed.timeout_targets.len(),
        observed.timeout_messages.len()
    );
    assert_eq!(
        observed.timeout_facts.len(),
        observed.timeout_messages.len()
    );
    assert_eq!(observed.kinds.get("message.created"), Some(&messages));
    assert_eq!(observed.kinds.get("channel.created"), Some(&channels));
    assert_eq!(observed.kinds.get("channel.joined"), Some(&5));
    assert_eq!(
        observed.kinds.get("reservation.acquired"),
        Some(&(channels - 1))
    );
    assert_eq!(
        observed.kinds.get("reservation.released"),
        Some(&(channels - 1))
    );
    assert!(!observed.kinds.contains_key("reservation.expired"));
    assert_eq!(
        observed.kinds.get("project.channel_policy_materialized"),
        Some(&1)
    );
    // This workload explicitly invokes the shared notification policy; these
    // expected populations do not imply that recording any event creates work.
    // five joins contribute 20 non-echo deliveries, each original body four,
    // each empty side-channel reservation pair eight, each timeout one author.
    assert_eq!(
        observed.events as usize,
        12 + messages + 3 * (channels - 1) + observed.timeout_messages.len()
    );
    assert_eq!(
        stats.deliveries as usize,
        20 + 4 * messages + 8 * (channels - 1) + observed.timeout_messages.len()
    );
    assert_eq!(snapshot.head().get(), observed.head);
    let mut cursor = Cursor::START;
    let mut digest = Sha256::new();
    let mut count = 0;
    loop {
        let rows = snapshot.events(cursor, 200, None).await.unwrap();
        if rows.is_empty() {
            break;
        }
        for event in rows {
            assert!(event.seq.cursor() > cursor && event.seq.get() <= observed.head);
            count += 1;
            cursor = event.seq.cursor();
            hash(&mut digest, &event);
        }
    }
    assert_eq!(count, observed.events);
    assert_eq!(cursor.get(), observed.head);
    assert_eq!(finish(digest), observed.digest);
    let mut seen = HashSet::<Uuid>::new();
    let mut cursor = Cursor::START;
    loop {
        let rows = snapshot
            .messages(&MessageFilter::default(), cursor, 200)
            .await
            .unwrap();
        if rows.is_empty() {
            break;
        }
        for message in rows {
            assert!(seen.insert(message.id));
            cursor = message.creation_seq.cursor();
            let receipts = snapshot
                .recipient_receipts(message.id, Cursor::START, 200)
                .await
                .unwrap();
            assert_eq!(receipts.len(), 4);
            if !automatic_reads && let Some(fact) = observed.timeout_facts.get(&message.id) {
                assert_eq!(fact.author, message.author_id);
                assert!(fact.count > 0 && fact.count <= receipts.len() as u64);
                assert!(fact.recipients.len() as u64 <= fact.count);
                for (label, deadline) in &fact.recipients {
                    let row = receipts
                        .iter()
                        .find(|row| row.item.recipient == *label)
                        .unwrap();
                    let delivery = row.item.delivery.as_ref().unwrap();
                    assert_eq!(delivery.history.deadline_at, Some(*deadline));
                    assert!(delivery.history.timed_out_at.is_some());
                }
            }
            for row in receipts {
                assert_ne!(row.item.recipient.actor_id, message.author_id);
                let delivery = row
                    .item
                    .delivery
                    .as_ref()
                    .expect("actual original receiver");
                assert_eq!(delivery.state, DeliveryState::Delivered);
                assert!(delivery.history.delivered_at.is_some());
                assert!(delivery.history.timed_out_at.is_none());
                assert_eq!(
                    delivery.history.deadline_at,
                    Some(
                        delivery
                            .history
                            .processing_started_at
                            .unwrap()
                            .after_seconds(3_600)
                            .unwrap()
                    )
                );
                let kinds = row
                    .item
                    .reads
                    .iter()
                    .map(|read| read.kind)
                    .collect::<Vec<_>>();
                let expected = if automatic_reads {
                    vec![
                        agentchat_core::model::ReadKind::Automatic,
                        agentchat_core::model::ReadKind::Explicit,
                    ]
                } else {
                    vec![agentchat_core::model::ReadKind::Explicit]
                };
                assert_eq!(kinds, expected);
            }
        }
    }
    assert_eq!(seen.len(), messages);
    let mut cursor = Cursor::START;
    let mut reservations = 0;
    loop {
        let rows = snapshot
            .reservations(
                None,
                true,
                agentchat_core::time::Clock::now(&agentchat_core::time::SystemClock),
                cursor,
                200,
            )
            .await
            .unwrap();
        if rows.is_empty() {
            break;
        }
        for group in rows {
            assert_eq!(group.state, agentchat_core::model::LeaseState::Released);
            assert!(group.released_at.is_some());
            assert_eq!(group.patterns.len(), 1);
            cursor = Cursor::new(group.generation).unwrap();
            reservations += 1;
        }
    }
    assert_eq!(reservations, channels - 1);
    json!({"statistics":stats,"head":observed.head,"event_digest_sha256":observed.digest,
        "event_kinds":observed.kinds,"complete_messages_checked":seen.len(),"each_message_read_by":4,
        "read_kinds_per_receiver":if automatic_reads {vec!["automatic","explicit"]} else {vec!["explicit"]},
        "coalesced_confirmation_timeouts":observed.timeout_messages.len(),
        "released_reservation_groups":reservations})
}

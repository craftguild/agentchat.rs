//! Real child exits without destructors around business and wake transactions.

#[path = "crash_recovery/completions.rs"]
mod completions;
#[path = "crash_recovery/groups.rs"]
mod groups;
#[path = "crash_recovery/launches.rs"]
mod launches;
#[path = "crash_recovery/read_sets.rs"]
mod read_sets;
#[path = "crash_recovery/ready_commits.rs"]
mod ready_commits;
#[path = "crash_recovery/reply_reads.rs"]
mod reply_reads;
#[path = "crash_recovery/settlement.rs"]
mod settlement;
mod support;
#[path = "crash_recovery/wake_reads.rs"]
mod wake_reads;

use std::{
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use agentchat_core::{
    Result,
    application::{Application, Mutation},
    delivery::{BatchState, HostReceipt, WakeBatch},
    model::*,
    ports::{IdempotencyRecord, Storage, Transaction},
    project::{ProjectIdentity, Scope},
    sequence::Sequence,
    time::Timestamp,
};
use agentchat_store::Store;
use async_trait::async_trait;
use support::{Fixture, SyntheticHost, TestClock};
use uuid::Uuid;

struct CrashStore {
    store: Arc<Store>,
    point: String,
}
static MESSAGE_STREAM_WRITES: AtomicUsize = AtomicUsize::new(0);
struct CrashTransaction<'a> {
    inner: Box<dyn Transaction + 'a>,
    point: String,
    depth: usize,
    processing_only: bool,
}
static GROUP_RELEASES: AtomicUsize = AtomicUsize::new(0);
static GROUP_MESSAGES: AtomicUsize = AtomicUsize::new(0);
static GROUP_PUBLICATIONS: AtomicUsize = AtomicUsize::new(0);
static GROUP_BATCHES: AtomicUsize = AtomicUsize::new(0);

fn cut(selected: &str, point: &str) {
    if selected == point {
        // exit skips Rust destructors and SQLite connection cleanup. The parent
        // must observe recovery from the on-disk WAL, not a Drop rollback.
        std::process::exit(86);
    }
}

#[async_trait]
impl Storage for CrashStore {
    async fn snapshot(&self) -> Result<Box<dyn agentchat_core::read::Snapshot>> {
        self.store.snapshot().await
    }
    fn project_id(&self) -> Uuid {
        self.store.identity().id
    }
    async fn begin(&self) -> Result<Box<dyn Transaction>> {
        Ok(Box::new(CrashTransaction {
            inner: self.store.begin().await?,
            point: self.point.clone(),
            depth: 0,
            processing_only: false,
        }))
    }
}

macro_rules! transaction {
    ($($name:ident($($arg:ident : $ty:ty),*) -> $ret:ty;)*) => {
        #[async_trait]
        impl Transaction for CrashTransaction<'_> {
            async fn save_launch(&mut self, launch: &Launch) -> Result<()> {
                self.inner.save_launch(launch).await?;
                cut(&self.point, if launch.retired_at.is_some() { "launch_after_retire" } else { "launch_after_start" });
                Ok(())
            }
            async fn cancel_deliveries(&mut self, agent: Uuid, at: Timestamp) -> Result<()> {
                self.inner.cancel_deliveries(agent, at).await?;
                cut(&self.point, "launch_after_cancel");
                Ok(())
            }
            async fn save_message_stream(&mut self, stream:&agentchat_core::delivery::MessageStream) -> Result<()> {
                self.inner.save_message_stream(stream).await?;
                self.processing_only = false;
                cut(&self.point,"message_after_stream");
                if self.point=="body_fail_second_stream" && MESSAGE_STREAM_WRITES.fetch_add(1,Ordering::SeqCst)==1 {
                    return Err(agentchat_core::Error::new(agentchat_core::ErrorCode::StorageError,"Synthetic second body acceptance storage failure."));
                }
                Ok(())
            }
            async fn savepoint(&mut self) -> Result<Box<dyn Transaction + '_>> {
                Ok(Box::new(CrashTransaction {inner:self.inner.savepoint().await?,point:self.point.clone(),depth:self.depth+1,processing_only:false}))
            }
            async fn rollback(self:Box<Self>) -> Result<()> {
                self.inner.rollback().await?;
                if self.depth>0 {cut(&self.point,"group_after_error_rollback");}
                Ok(())
            }
            $(async fn $name(&mut self,$($arg:$ty),*) -> Result<$ret> {
                self.inner.$name($($arg),*).await
            })*
            async fn wake_batch(&mut self,id:Uuid) -> Result<Option<WakeBatch>> {
                wake_reads::record(&self.point,wake_reads::Read::Batch);
                self.inner.wake_batch(id).await
            }
            async fn wake_batch_has_pending(&mut self,agent:Uuid,batch:Uuid) -> Result<bool> {
                wake_reads::record(&self.point,wake_reads::Read::Pending);
                self.inner.wake_batch_has_pending(agent,batch).await
            }
            async fn wake_items(&mut self,agent:Uuid,batch:Option<Uuid>) -> Result<Vec<agentchat_core::delivery::WakeItem>> {
                wake_reads::record(&self.point,wake_reads::Read::Items);
                self.inner.wake_items(agent,batch).await
            }
            async fn allocate_event(&mut self, kind:EventKind, now:Timestamp) -> Result<Event> {
                let event=self.inner.allocate_event(kind,now).await?;
                cut(&self.point,"after_event");
                Ok(event)
            }
            async fn allocate_dispatch(&mut self,kind:DispatchKind,channel:Option<Uuid>,now:Timestamp)->Result<Dispatch> {
                self.inner.allocate_dispatch(kind,channel,now).await
            }
            async fn publish_dispatch(&mut self,header:&Dispatch,payload:&DispatchPayload,deliveries:&[Delivery])->Result<()> {
                self.inner.publish_dispatch(header,payload,deliveries).await?;
                if header.kind==DispatchKind::Message {
                    cut(&self.point,"after_state");
                    if self.depth>0 && GROUP_MESSAGES.fetch_add(1,Ordering::SeqCst)==1 {cut(&self.point,"group_second_state");}
                }
                Ok(())
            }
            async fn publish_event(&mut self,event:&Event)->Result<()> {
                self.inner.publish_event(event).await?;
                cut(&self.point,"after_deliveries");
                if self.depth>0 && GROUP_PUBLICATIONS.fetch_add(1,Ordering::SeqCst)==1 {cut(&self.point,"group_second_deliveries");}
                Ok(())
            }
            async fn save_wake_batch(&mut self, batch:&WakeBatch) -> Result<()> {
                self.inner.save_wake_batch(batch).await?;
                cut(&self.point,"after_batch");
                if self.depth>0 && GROUP_BATCHES.fetch_add(1,Ordering::SeqCst)==1 {
                    cut(&self.point,"completion_second_batch");
                    if self.point=="completion_fail_second_batch" {return Err(agentchat_core::Error::new(agentchat_core::ErrorCode::StorageError,"Synthetic second callback storage failure."));}
                    if self.point=="completion_corrupt_second_batch" {return Err(agentchat_core::Error::new(agentchat_core::ErrorCode::IntegrityError,"Synthetic second callback storage corruption."));}
                    if self.point=="completion_io_second_batch" {return Err(agentchat_core::Error::new(agentchat_core::ErrorCode::IoError,"Synthetic second callback storage I/O failure."));}
                    if self.point=="completion_pause_second_batch" {completions::pause();}
                }
                Ok(())
            }
            async fn save_delivery(&mut self, delivery:&Delivery) -> Result<()> {
                self.inner.save_delivery(delivery).await?;
                self.processing_only = delivery.state == DeliveryState::Processing && delivery.attempts == 0;
                if matches!(delivery.state, DeliveryState::Delivered | DeliveryState::DeliveredLate) {
                    cut(&self.point,"settlement_after_delivery");
                    if self.point=="settlement_fail_after_delivery" { return Err(agentchat_core::Error::new(agentchat_core::ErrorCode::StorageError,"Synthetic completion write failure.")); }
                }
                cut(&self.point,"after_assignment");
                Ok(())
            }
            async fn delivery(&mut self, id:Uuid) -> Result<Option<Delivery>> {
                self.inner.delivery(id).await
            }
            async fn deliveries(&mut self, ids:&[Uuid]) -> Result<Vec<Delivery>> {
                self.inner.deliveries(ids).await
            }
            async fn save_delivery_timeout_notice(&mut self, message: Uuid, event: Uuid) -> Result<()> {
                self.inner.save_delivery_timeout_notice(message, event).await?;
                cut(&self.point, "after_timeout_notice");
                Ok(())
            }
            async fn message(&mut self,id:Uuid) -> Result<Option<Message>> {
                read_sets::record(&self.point,read_sets::Work::SingleMessage);
                self.inner.message(id).await
            }
            async fn messages(&mut self,ids:&[Uuid]) -> Result<Vec<Message>> {
                read_sets::record(&self.point,read_sets::Work::Messages);
                self.inner.messages(ids).await
            }
            async fn receipt(&mut self,message:Uuid,agent:Uuid) -> Result<Option<Receipt>> {
                read_sets::record(&self.point,read_sets::Work::SingleReceipt);
                self.inner.receipt(message,agent).await
            }
            async fn receipts(&mut self,agent:Uuid,ids:&[Uuid]) -> Result<Vec<Receipt>> {
                read_sets::record(&self.point,read_sets::Work::Receipts);
                self.inner.receipts(agent,ids).await
            }
            async fn insert_receipt(&mut self,receipt:&Receipt) -> Result<()> {
                read_sets::record(&self.point,read_sets::Work::SingleInsert);
                self.inner.insert_receipt(receipt).await
            }
            async fn insert_receipts(&mut self,receipts:&[Receipt]) -> Result<()> {
                read_sets::record(&self.point,read_sets::Work::InsertSet);
                cut(&self.point,"read_before_insert");
                self.inner.insert_receipts(receipts).await?;
                cut(&self.point,"read_after_insert");
                if self.point=="read_fail_after_insert" {return Err(agentchat_core::Error::new(agentchat_core::ErrorCode::StorageError,"Synthetic first-read storage failure."));}
                Ok(())
            }
            async fn commit(self:Box<Self>) -> Result<()> {
                if self.processing_only { cut(&self.point,"processing_before_commit"); }
                else { cut(&self.point,"before_commit"); }
                if self.depth==0 && !self.processing_only {
                    cut(&self.point,"group_before_outer_commit");
                    if self.point=="group_reject_outer_commit" {return Err(agentchat_core::Error::new(agentchat_core::ErrorCode::StorageError,"Synthetic outer commit failure."));}
                } else if self.depth>0 {cut(&self.point,&format!("group_before_release_{}",GROUP_RELEASES.load(Ordering::SeqCst)+1));}
                let processing_only = self.processing_only;
                self.inner.commit().await?;
                if processing_only { cut(&self.point,"processing_after_commit"); }
                else { cut(&self.point,"after_commit"); }
                if self.depth==0 && !processing_only {cut(&self.point,"group_after_outer_commit");}
                else if self.depth>0 {cut(&self.point,&format!("group_after_release_{}",GROUP_RELEASES.fetch_add(1,Ordering::SeqCst)+1));}
                Ok(())
            }
        }
    }
}

transaction! {
    receipt_kind(message:Uuid, actor:Uuid, kind:ReadKind) -> Option<Receipt>;
    event(id:Uuid) -> Option<Event>;
    dispatch(id:Uuid) -> Option<Dispatch>;
    due_deliveries(now:Timestamp) -> Vec<Delivery>;
    delivery_timeout_notice(message:Uuid) -> Option<Uuid>;
    expired_recipients(message:Uuid) -> ExpiredRecipients;
    cancel_author_timeout_notices(actor:Uuid) -> ();
    author_timeout_notice_allowed(actor:Uuid, created:Sequence) -> bool;
    message_stream(generation:Uuid) -> Option<agentchat_core::delivery::MessageStream>;
    next_message(agent:Uuid, after:Sequence) -> Option<agentchat_core::delivery::MessageItem>;
    next_native(agent:Uuid, after:Sequence) -> Option<agentchat_core::delivery::NativeItem>;
    retained_native(agent:Uuid, delivery:Uuid, seq:Sequence) -> Option<agentchat_core::delivery::NativeItem>;
    latest_launch(agent:Uuid) -> Option<agentchat_core::model::Launch>;
    launch(generation:Uuid) -> Option<agentchat_core::model::Launch>;
    active_launches(after:Option<Uuid>) -> Vec<agentchat_core::model::Launch>;
    outstanding_wake(agent:Uuid,incarnation:Uuid) -> Option<agentchat_core::delivery::WakeBatch>;
    pending_delivery_count(agent:Uuid) -> u64;
    retire_wake_incarnations(agent:Uuid,current:Uuid) -> ();
    idempotency(id:Uuid) -> Option<IdempotencyRecord>;
    save_idempotency(record:&IdempotencyRecord) -> ();
    agent(id:Uuid) -> Option<Agent>;
    conversation(host:HostKind, id:&str) -> Option<Agent>;
    handle(handle:&str) -> Option<Agent>;
    joined_agents() -> Vec<Agent>;
    save_agent(agent:&Agent) -> ();
    actor(id:Uuid) -> Option<agentchat_core::model::Actor>;
    user() -> Option<agentchat_core::model::Actor>;
    save_actor(actor:&agentchat_core::model::Actor) -> ();
    joined_actors() -> Vec<agentchat_core::model::Actor>;
    session(agent:Uuid) -> Option<Session>;
    save_session(session:&Session) -> ();
    disconnect_sessions() -> ();
    channel(name:&str) -> Option<Channel>;
    channels() -> Vec<Channel>;
    channel_policy() -> Option<agentchat_core::project::ProjectChannelPolicy>;
    save_channel_policy(policy:&agentchat_core::project::ProjectChannelPolicy) -> ();
    receipt_settings() -> agentchat_core::project::ProjectReceiptSettings;
    save_receipt_settings(settings:&agentchat_core::project::ProjectReceiptSettings) -> ();
    insert_channel(channel:&Channel) -> ();
    add_member(channel:Uuid, agent:Uuid, seq:Sequence) -> ();
    is_member(channel:Uuid, agent:Uuid) -> bool;
    membership_boundary(channel:Uuid, agent:Uuid) -> Option<Sequence>;
    remove_member(channel:Uuid, agent:Uuid) -> ();
    cancel_channel_deliveries(channel:Uuid, agent:Uuid, at:Timestamp) -> ();
    channel_member_count(channel:Uuid) -> u64;
    channel_members(channel:Uuid) -> Vec<Agent>;
    remove_memberships(agent:Uuid) -> ();
    insert_mention(mention:&Mention) -> ();
    reservation(id:Uuid) -> Option<Reservation>;
    active_reservations(after:Option<Uuid>) -> Vec<Reservation>;
    save_reservation(reservation:&Reservation) -> ();
}

#[tokio::test]
#[ignore = "subprocess helper; the parent runs all crash cuts explicitly"]
async fn crash_child() {
    let path = std::env::var_os("AGENTCHAT_TEST_CRASH_DB").expect("child-only database");
    let context: Context =
        serde_json::from_str(&std::env::var("AGENTCHAT_TEST_CRASH_CONTEXT").unwrap()).unwrap();
    let request = Uuid::parse_str(&std::env::var("AGENTCHAT_TEST_CRASH_REQUEST").unwrap()).unwrap();
    let point = std::env::var("AGENTCHAT_TEST_CRASH_POINT").unwrap();
    let store = Arc::new(
        Store::open(
            std::path::PathBuf::from(path),
            ProjectIdentity {
                id: context.project_id,
                scope: Scope::Directory,
            },
        )
        .await
        .unwrap(),
    );
    let clock = Arc::new(TestClock::fixed());
    if std::env::var("AGENTCHAT_TEST_CRASH_ACTION").as_deref() == Ok("timeout") {
        clock.advance(2);
    }
    if std::env::var("AGENTCHAT_TEST_CRASH_ACTION").as_deref() == Ok("read_set") {
        clock.advance(2);
    }
    let app = Application::new(
        Arc::new(CrashStore {
            store,
            point: point.clone(),
        }),
        clock,
        Arc::new(SyntheticHost::default()),
    );
    if std::env::var("AGENTCHAT_TEST_CRASH_ACTION").as_deref() == Ok("read_set") {
        read_sets::run(&app, &context, request, &point).await;
    } else if std::env::var("AGENTCHAT_TEST_CRASH_ACTION").as_deref() == Ok("read_reply") {
        reply_reads::run(&app, &context, request).await;
    } else if std::env::var("AGENTCHAT_TEST_CRASH_ACTION").as_deref() == Ok("ready_commits") {
        ready_commits::run(&app, &context, request, &point).await;
    } else if std::env::var("AGENTCHAT_TEST_CRASH_ACTION").as_deref() == Ok("completion_group") {
        completions::run(&app, &point).await;
    } else if std::env::var("AGENTCHAT_TEST_CRASH_ACTION").as_deref() == Ok("group") {
        let outcome = app.execute_group(groups::requests(&context, request)).await;
        if point == "group_reject_outer_commit" {
            assert_eq!(
                outcome.unwrap_err().code,
                agentchat_core::ErrorCode::StorageError
            );
            std::process::exit(86);
        }
        let outcomes = outcome.unwrap();
        assert!(outcomes[0].is_ok() && outcomes[1].is_err() && outcomes[2].is_ok());
    } else if std::env::var("AGENTCHAT_TEST_CRASH_ACTION").as_deref() == Ok("wake") {
        let dispatch = app.prepare_wake(&context).await.unwrap().unwrap();
        // This is a test-owned receiver receipt, not evidence of real host or
        // model acceptance. Sync it to distinguish the two external-I/O cuts.
        let marker = std::env::var_os("AGENTCHAT_TEST_CRASH_MARKER").unwrap();
        let mut file = std::fs::File::create(marker).unwrap();
        serde_json::to_writer(&mut file, &dispatch.hint).unwrap();
        file.sync_all().unwrap();
        cut(&point, "after_native_accept");
        app.finish_wake(&context, &dispatch, Ok(HostReceipt::default()))
            .await
            .unwrap();
        cut(&point, "after_callback");
    } else if std::env::var("AGENTCHAT_TEST_CRASH_ACTION").as_deref() == Ok("timeout") {
        app.maintain().await.unwrap();
    } else {
        app.execute(request, &context, post()).await.unwrap();
    }
    panic!("Expected a hard process exit at the selected cut.");
}

fn post() -> Mutation {
    Mutation::Post {
        channel: Some("dev".into()),
        body: "@bob survive the WAL crash cut".into(),
    }
}

#[tokio::test]
async fn crash_cuts_preserve_atomic_state_events_inboxes_and_retry_results() {
    for point in [
        "after_event",
        "after_state",
        "after_deliveries",
        "before_commit",
        "after_commit",
    ] {
        let f = Fixture::new().await;
        let a = f.context();
        let b = f.context();
        f.join(&a, "alice").await;
        f.join(&b, "bob").await;
        let before = f.counts().await;
        let request = Uuid::new_v4();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "crash_child"])
            .env(
                "AGENTCHAT_TEST_CRASH_DB",
                f.root.path().join("agentchat.sqlite3"),
            )
            .env(
                "AGENTCHAT_TEST_CRASH_CONTEXT",
                serde_json::to_string(&a).unwrap(),
            )
            .env("AGENTCHAT_TEST_CRASH_REQUEST", request.to_string())
            .env("AGENTCHAT_TEST_CRASH_POINT", point)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("Crash child did not reach {point}");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(status.code(), Some(86), "{point}");
        f.store.integrity_check().await.unwrap();
        let recovered = Store::open(
            f.root.path().join("agentchat.sqlite3"),
            f.store.identity().clone(),
        )
        .await
        .unwrap();
        recovered.integrity_check().await.unwrap();
        recovered.close().await.unwrap();
        if point != "after_commit" {
            assert_eq!(f.counts().await, before, "{point}");
        }
        let result = f.app.execute(request, &a, post()).await.unwrap();
        assert_eq!(
            result.event_head.is_none(),
            point == "after_commit",
            "{point}"
        );
        assert_eq!(f.count("dispatch_messages").await, 1, "{point}");
        assert_eq!(f.count("delivery_timeout_notices").await, 0, "{point}");
        let rows = f.rows("dispatch_messages").await;
        let message = rows[0].try_get::<String>("", "dispatch_id").unwrap();
        assert_eq!(result.result["message_id"], message);
        let events = f.rows("events").await;
        let event = events
            .iter()
            .find(|r| {
                r.try_get::<Option<String>>("", "message_id")
                    .unwrap()
                    .as_deref()
                    == Some(message.as_str())
            })
            .unwrap();
        assert!(event.try_get::<bool>("", "sealed").unwrap());
        // The sealed audit event retains Alice; only Bob has receiver work.
        assert_eq!(
            f.rows("deliveries")
                .await
                .iter()
                .filter(|r| r.try_get::<String>("", "dispatch_id").unwrap() == message)
                .count(),
            1
        );
        let before_retry = f.counts().await;
        assert_eq!(
            f.app.execute(request, &a, post()).await.unwrap().result,
            result.result
        );
        assert_eq!(f.counts().await, before_retry);
    }
}

#[tokio::test]
async fn commit_refuses_an_allocated_event_that_was_never_published() {
    let f = Fixture::new().await;
    let before = f.counts().await;
    let mut tx = f.store.begin().await.unwrap();
    tx.allocate_event(
        EventKind::MessageCreated,
        Timestamp::parse("2026-10-15T00:00:00.000Z").unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        tx.commit().await.unwrap_err().code,
        agentchat_core::ErrorCode::IntegrityError
    );
    assert_eq!(f.counts().await, before);
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn wake_crash_cuts_preserve_claim_atomicity_and_reconnect_delivery_ids() {
    for point in [
        "after_batch",
        "after_assignment",
        "before_commit",
        "after_commit",
        "after_native_accept",
        "after_callback",
    ] {
        let f = Fixture::new().await;
        let a = f.context();
        let b = f.context();
        f.join(&a, "alice").await;
        let bob = f.join(&b, "bob").await;
        let message = f.post(&a, "@bob durable native presentation").await;
        let delivery_id = f
            .delivery(support::id(&message.result, "message_id"), bob)
            .await;
        let before = f.counts().await;
        let marker = f.root.path().join("synthetic-receiver.json");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "crash_child"])
            .env("AGENTCHAT_TEST_CRASH_ACTION", "wake")
            .env(
                "AGENTCHAT_TEST_CRASH_DB",
                f.root.path().join("agentchat.sqlite3"),
            )
            .env(
                "AGENTCHAT_TEST_CRASH_CONTEXT",
                serde_json::to_string(&b).unwrap(),
            )
            .env("AGENTCHAT_TEST_CRASH_REQUEST", Uuid::new_v4().to_string())
            .env("AGENTCHAT_TEST_CRASH_POINT", point)
            .env("AGENTCHAT_TEST_CRASH_MARKER", &marker)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("Wake child did not reach {point}");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(status.code(), Some(86), "{point}");
        let recovered = Arc::new(
            Store::open(
                f.root.path().join("agentchat.sqlite3"),
                f.store.identity().clone(),
            )
            .await
            .unwrap(),
        );
        recovered.integrity_check().await.unwrap();
        let mut tx = recovered.begin().await.unwrap();
        let delivery = tx.delivery(delivery_id).await.unwrap().unwrap();
        let committed = matches!(
            point,
            "after_commit" | "after_native_accept" | "after_callback"
        );
        assert_eq!(delivery.attempts, u32::from(committed), "{point}");
        assert_eq!(delivery.wake_batch_id.is_some(), committed, "{point}");
        assert_eq!(
            delivery.state,
            if committed {
                DeliveryState::Processing
            } else {
                DeliveryState::Pending
            }
        );
        assert!(delivery.history.delivered_at.is_none());
        if let Some(id) = delivery.wake_batch_id {
            let batch = tx.wake_batch(id).await.unwrap().unwrap();
            assert_eq!(batch.attempts, 1);
            assert_eq!(
                batch.state,
                if point == "after_callback" {
                    BatchState::Inflight
                } else {
                    BatchState::Pending
                }
            );
        }
        drop(tx);
        let mut after = f.counts().await;
        assert_eq!(after["wake_batches"], usize::from(committed));
        assert_eq!(
            after["delivery_state_transitions"],
            before["delivery_state_transitions"] + usize::from(committed)
        );
        after.insert("wake_batches".into(), before["wake_batches"]);
        after.insert(
            "delivery_state_transitions".into(),
            before["delivery_state_transitions"],
        );
        assert_eq!(
            after, before,
            "Wake control must not change business data at {point}"
        );
        assert_eq!(
            marker.exists(),
            matches!(point, "after_native_accept" | "after_callback")
        );
        if marker.exists() {
            let hint: agentchat_core::delivery::WakeHint =
                serde_json::from_reader(std::fs::File::open(&marker).unwrap()).unwrap();
            assert_eq!(hint.events[0].delivery_id, delivery_id);
        }
        let app = Application::new(recovered.clone(), f.clock.clone(), f.host.clone());
        app.invalidate_connections().await.unwrap();
        assert!(app.prepare_wake(&b).await.unwrap().is_none());
        let fresh = Context {
            context_id: Uuid::new_v4(),
            incarnation: Uuid::new_v4(),
            ..b.clone()
        };
        assert!(app.attach(&fresh).await.unwrap().unwrap().joined);
        let dispatch = app.prepare_wake(&fresh).await.unwrap().unwrap();
        assert_eq!(dispatch.attempt, 1);
        assert_eq!(dispatch.hint.events[0].delivery_id, delivery_id);
        assert_ne!(Some(dispatch.hint.batch_id), delivery.wake_batch_id);
        assert_eq!(f.count("read_receipts").await, 0);
        recovered.integrity_check().await.unwrap();
    }
}

#[tokio::test]
async fn timeout_crash_cuts_commit_history_notice_and_deliveries_together_once() {
    for point in [
        "after_event",
        "after_deliveries",
        "after_timeout_notice",
        "before_commit",
        "after_commit",
    ] {
        let f = Fixture::new().await;
        let a = f.context();
        let b = f.context();
        f.join(&a, "alice").await;
        let gate = support::native::Gate::new(&f);
        gate.bind(&b);
        let native = gate.app(&f);
        native.start_launch(&b, Some("bob")).await.unwrap();
        f.timeout(1).await;
        f.run(&a, post()).await;
        native.prepare_message(&b).await.unwrap().unwrap();
        let before = f.counts().await;
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "crash_child"])
            .env("AGENTCHAT_TEST_CRASH_ACTION", "timeout")
            .env(
                "AGENTCHAT_TEST_CRASH_DB",
                f.root.path().join("agentchat.sqlite3"),
            )
            .env(
                "AGENTCHAT_TEST_CRASH_CONTEXT",
                serde_json::to_string(&a).unwrap(),
            )
            .env("AGENTCHAT_TEST_CRASH_REQUEST", Uuid::new_v4().to_string())
            .env("AGENTCHAT_TEST_CRASH_POINT", point)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("Timeout child did not reach {point}");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(status.code(), Some(86));
        let recovered = Arc::new(
            Store::open(
                f.root.path().join("agentchat.sqlite3"),
                f.store.identity().clone(),
            )
            .await
            .unwrap(),
        );
        recovered.integrity_check().await.unwrap();
        if point != "after_commit" {
            assert_eq!(f.counts().await, before);
        }
        f.clock.advance(2);
        let app = Application::new(recovered.clone(), f.clock.clone(), f.host.clone());
        assert_eq!(
            app.maintain().await.unwrap().delivery_confirmation_timeouts,
            usize::from(point != "after_commit")
        );
        assert_eq!(
            app.maintain().await.unwrap().delivery_confirmation_timeouts,
            0
        );
        let events = f.rows("events").await;
        let timeouts = events
            .iter()
            .filter(|r| {
                r.try_get::<String>("", "kind").unwrap() == "message.delivery_confirmation_timeout"
            })
            .collect::<Vec<_>>();
        assert_eq!(timeouts.len(), 1);
        let audit_id = timeouts[0].try_get::<String>("", "event_id").unwrap();
        let watch = f.rows("delivery_timeout_notices").await;
        let notice = watch[0]
            .try_get::<String>("", "notice_dispatch_id")
            .unwrap();
        assert_ne!(notice, audit_id);
        assert!(
            f.rows("dispatch_notices")
                .await
                .iter()
                .any(|r| r.try_get::<String>("", "dispatch_id").unwrap() == notice)
        );
        assert_eq!(
            f.rows("deliveries")
                .await
                .iter()
                .filter(|r| r.try_get::<String>("", "dispatch_id").unwrap() == notice)
                .count(),
            1
        );
        recovered.integrity_check().await.unwrap();
    }
}

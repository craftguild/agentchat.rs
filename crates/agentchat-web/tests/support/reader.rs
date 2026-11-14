//! Fixed-capacity injected fixture; production snapshot policy is tested against SQLite/RPC.

use agentchat_core::{
    Error, ErrorCode, Result,
    model::{Agent, Channel, HostKind, Message, Phase},
    read::{
        ActorView, AgentView, ChangeBatch, ChangeRequest, ChannelListCondition, ChannelPage,
        ChannelView, Checkpoint, IdentityLabel, MessageDetail, MessageView, MessageWindow,
        ObservedMessage, Page, PageRequest, PinnedRoot, RecipientReceipt, ViewerChange,
        ViewerChangeKind, WindowRequest, requests,
    },
    sequence::{Cursor, Sequence},
    time::Timestamp,
};
use agentchat_protocol::viewer::{ChangeObserver, ViewerReader};
use async_trait::async_trait;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::sync::watch;
use uuid::Uuid;

pub struct Fixture {
    pub project: Uuid,
    pub channel: Channel,
    pub rows: Mutex<Vec<(Uuid, ObservedMessage)>>,
    pub channels: Mutex<Vec<Channel>>,
    pub receipt_rows: Mutex<Vec<RecipientReceipt>>,
    pub extra_members: Mutex<Vec<ActorView>>,
    pub hints: watch::Sender<usize>,
    pub starts: AtomicUsize,
    pub drops: AtomicUsize,
    pub fail_setup: AtomicBool,
    pub fail_source: AtomicBool,
    sequence: AtomicUsize,
    epoch: AtomicUsize,
}
impl Fixture {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            project: Uuid::from_u128(1),
            channel: Channel {
                id: Uuid::from_u128(2),
                name: "dev".into(),
                creator_id: None,
                creation_seq: Sequence::new(1).unwrap(),
            },
            rows: Mutex::default(),
            channels: Mutex::default(),
            receipt_rows: Mutex::default(),
            extra_members: Mutex::default(),
            hints: watch::channel(1).0,
            starts: AtomicUsize::new(0),
            drops: AtomicUsize::new(0),
            sequence: AtomicUsize::new(1),
            epoch: AtomicUsize::new(0),
            fail_setup: AtomicBool::new(false),
            fail_source: AtomicBool::new(false),
        })
    }
    pub fn append(&self, body: String) {
        self.post("dev", None, body);
    }
    pub fn reply(&self, root: Uuid, body: String) {
        let channel = self
            .rows
            .lock()
            .unwrap()
            .iter()
            .find(|(_, row)| row.message.message.id == root)
            .unwrap()
            .1
            .message
            .channel
            .clone();
        self.post(&channel, Some(root), body);
    }
    pub fn post(&self, channel: &str, root: Option<Uuid>, body: String) {
        assert!(body.len() <= 65_536);
        let mut rows = self.rows.lock().unwrap();
        assert!(rows.len() < 512);
        let n = self.sequence.fetch_add(2, Ordering::SeqCst) as i64 + 1;
        // Fixture identity follows retained content, independently of Header/audit allocation.
        let id = Uuid::from_u128(rows.len() as u128 + 12);
        let channel = if channel == "dev" {
            self.channel.clone()
        } else {
            self.channels
                .lock()
                .unwrap()
                .iter()
                .find(|row| row.name == channel)
                .unwrap()
                .clone()
        };
        if let Some(root) = root {
            assert!(rows.iter().any(|(_, row)| row.message.message.id == root
                && row.message.message.root_message_id.is_none()
                && row.message.message.channel_id == channel.id));
        }
        rows.push((
            self.cp(n + 1).event_id.unwrap(),
            ObservedMessage {
                message: MessageView {
                    message: Message {
                        id,
                        channel_id: channel.id,
                        author_id: Uuid::from_u128(3),
                        root_message_id: root,
                        body,
                        created_at: Timestamp::parse("2026-10-24T00:00:00.000Z").unwrap(),
                        creation_seq: Sequence::new(n).unwrap(),
                    },
                    thread_id: root.unwrap_or(id),
                    author: IdentityLabel {
                        kind: agentchat_core::model::ActorKind::Agent,
                        actor_id: Uuid::from_u128(3),
                        handle: "alice".into(),
                    },
                    channel: channel.name,
                },
                reply_count: 0,
            },
        ));
        self.hints.send_replace((n + 1) as usize);
    }
    pub fn add_channel(&self, name: String) {
        let _rows = self.rows.lock().unwrap();
        let mut channels = self.channels.lock().unwrap();
        assert!(channels.len() < 128 && name.len() <= 64);
        let n = self.sequence.fetch_add(1, Ordering::SeqCst) as i64 + 1;
        channels.push(Channel {
            id: Uuid::from_u128(n as u128 + 2000),
            name,
            creator_id: None,
            creation_seq: Sequence::new(n).unwrap(),
        });
        self.hints.send_replace(n as usize);
    }
    pub fn restore(&self) {
        let _rows = self.rows.lock().unwrap();
        self.epoch.fetch_add(1, Ordering::SeqCst);
        self.hints
            .send_replace(self.sequence.load(Ordering::SeqCst));
    }
    pub fn reset(&self) {
        let mut rows = self.rows.lock().unwrap();
        rows.clear();
        self.channels.lock().unwrap().clear();
        self.receipt_rows.lock().unwrap().clear();
        self.extra_members.lock().unwrap().clear();
        self.epoch.fetch_add(1, Ordering::SeqCst);
        self.sequence.store(1, Ordering::SeqCst);
        self.fail_setup.store(false, Ordering::SeqCst);
        self.fail_source.store(false, Ordering::SeqCst);
        self.hints.send_replace(1);
    }
    pub fn cp(&self, n: i64) -> Checkpoint {
        let base = if n == 1 { 4 } else { n as u128 + 1000 };
        Checkpoint {
            project_id: self.project,
            seq: Cursor::new(n).unwrap(),
            event_id: (n != 0).then(|| {
                Uuid::from_u128(base + self.epoch.load(Ordering::SeqCst) as u128 * 1_000_000)
            }),
        }
    }
    pub fn current(&self) -> Checkpoint {
        self.cp(*self.hints.borrow() as i64)
    }
    fn validate(&self, cp: Checkpoint) -> Result<()> {
        if cp.project_id != self.project
            || cp != self.cp(cp.seq.get())
            || cp.seq > self.current().seq
        {
            let mut error = Error::new(
                ErrorCode::InvalidArgument,
                "Invalidated fixture checkpoint.",
            );
            error.details = serde_json::json!({"reset_required":true});
            return Err(error);
        }
        Ok(())
    }
    fn projected(
        &self,
        row: &ObservedMessage,
        rows: &[(Uuid, ObservedMessage)],
        through: Cursor,
    ) -> ObservedMessage {
        let mut row = row.clone();
        row.reply_count = rows
            .iter()
            .filter(|(_, reply)| {
                reply.message.message.root_message_id == Some(row.message.thread_id)
                    && reply.message.message.creation_seq.cursor() <= through
            })
            .count() as u64;
        row
    }
    fn window(
        &self,
        channel: &str,
        root: Option<Uuid>,
        input: WindowRequest,
    ) -> Result<MessageWindow> {
        let rows = self.rows.lock().unwrap();
        let snapshot = input.snapshot.unwrap_or_else(|| self.current());
        self.validate(snapshot)?;
        let candidates: Vec<_> = rows
            .iter()
            .map(|(_, row)| row)
            .filter(|row| {
                row.message.channel == channel
                    && row.message.message.root_message_id == root
                    && row.message.message.creation_seq.cursor() <= snapshot.seq
                    && input
                        .before
                        .is_none_or(|before| row.message.message.creation_seq.cursor() < before)
            })
            .collect();
        let mut selected = Vec::new();
        let mut bytes = 1024;
        for row in candidates.iter().rev() {
            let row = self.projected(row, &rows, snapshot.seq);
            let size = serde_json::to_vec(&row).unwrap().len();
            if selected.len() == input.limit.unwrap_or(50) || bytes + size > 768 * 1024 {
                break;
            }
            bytes += size;
            selected.push(row);
        }
        selected.reverse();
        let before = selected
            .first()
            .map(|row| row.message.message.creation_seq.cursor());
        let more = candidates.len() > selected.len();
        Ok(MessageWindow {
            snapshot,
            items: selected,
            next_before: before.filter(|_| more),
        })
    }
    fn batch(&self, request: ChangeRequest) -> Result<ChangeBatch> {
        let rows = self.rows.lock().unwrap();
        self.validate(request.after)?;
        let head = request.through.unwrap_or_else(|| self.current());
        self.validate(head)?;
        let mut candidates = Vec::new();
        if request.after.seq == Cursor::START && head.seq.get() >= 1 {
            candidates.push(ViewerChange {
                event_id: self.cp(1).event_id.unwrap(),
                seq: self.channel.creation_seq,
                change: ViewerChangeKind::ChannelCreated {
                    channel: self.channel.clone(),
                },
            });
        }
        for channel in self.channels.lock().unwrap().iter() {
            if channel.creation_seq.cursor() > request.after.seq
                && channel.creation_seq.cursor() <= head.seq
            {
                candidates.push(ViewerChange {
                    event_id: self.cp(channel.creation_seq.get()).event_id.unwrap(),
                    seq: channel.creation_seq,
                    change: ViewerChangeKind::ChannelCreated {
                        channel: channel.clone(),
                    },
                });
            }
        }
        for (_, raw) in rows.iter() {
            let seq = Sequence::new(raw.message.message.creation_seq.get() + 1).unwrap();
            if seq.cursor() > request.after.seq && seq.cursor() <= head.seq {
                candidates.push(ViewerChange {
                    event_id: self.cp(seq.get()).event_id.unwrap(),
                    seq,
                    change: ViewerChangeKind::MessageCreated {
                        message: self.projected(raw, &rows, head.seq),
                    },
                });
            }
        }
        candidates.sort_by_key(|row| row.seq);
        let mut items = Vec::new();
        let mut bytes = 1024;
        for candidate in candidates {
            let size = serde_json::to_vec(&candidate).unwrap().len();
            if items.len() == request.limit.unwrap_or(50) || bytes + size > 768 * 1024 {
                break;
            }
            bytes += size;
            items.push(candidate);
        }
        let scan = items
            .last()
            .map(|row| self.cp(row.seq.get()))
            .unwrap_or(request.after);
        Ok(ChangeBatch {
            after: request.after,
            scan,
            head,
            has_more: scan.seq < head.seq,
            items,
        })
    }
}

pub struct Reader(pub Arc<Fixture>);
impl std::ops::Deref for Reader {
    type Target = Fixture;
    fn deref(&self) -> &Fixture {
        &self.0
    }
}
#[async_trait]
impl ViewerReader for Reader {
    async fn channel_members(&self, request: requests::ChannelMembers) -> Result<Page<ActorView>> {
        if request.channel != self.channel.name
            && !self
                .channels
                .lock()
                .unwrap()
                .iter()
                .any(|c| c.name == request.channel)
        {
            return Err(Error::new(ErrorCode::NotFound, "Channel not found"));
        }
        let mut page = self.agents(request.page).await?;
        page.items.retain(|agent| agent.agent.joined);
        Ok(Page {
            items: page
                .items
                .into_iter()
                .map(|agent| ActorView {
                    actor: agent.agent.into(),
                    presence: Some(agent.presence),
                    connected: Some(agent.connected),
                    incarnation: agent.incarnation,
                })
                .chain(self.extra_members.lock().unwrap().clone())
                .collect(),
            next_cursor: page.next_cursor,
            scan_cursor: page.scan_cursor,
            head: page.head,
        })
    }
    async fn message(&self, request: requests::Message) -> Result<MessageDetail> {
        let limit = request.receipts.limit()?;
        let rows = self.rows.lock().unwrap();
        let message = rows
            .iter()
            .find(|(_, row)| row.message.message.id == request.message_id)
            .ok_or_else(|| Error::new(ErrorCode::NotFound, "Message not found"))?
            .1
            .message
            .clone();
        let receipts = self.receipt_rows.lock().unwrap();
        assert!(receipts.len() <= 512);
        let start = usize::try_from(request.receipts.after.get()).unwrap();
        let items = receipts
            .iter()
            .skip(start)
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        let end = start + items.len();
        let more = end < receipts.len();
        let head = self.current().seq;
        let scan = if more {
            Cursor::new(end as i64)?
        } else {
            head.max(request.receipts.after)
        };
        Ok(MessageDetail {
            message,
            receipts: Page {
                items,
                next_cursor: more.then_some(scan),
                scan_cursor: scan,
                head,
            },
        })
    }
    async fn agents(&self, _: PageRequest) -> Result<Page<AgentView>> {
        let head = self.current().seq;
        Ok(Page {
            items: [
                (3, "alice", true, HostKind::Codex),
                (4, "bob", false, HostKind::Claude),
            ]
            .into_iter()
            .map(|(id, name, joined, host)| AgentView {
                agent: Agent {
                    id: Uuid::from_u128(id),
                    handle: name.into(),
                    host,
                    host_session_id: name.into(),
                    joined,
                    first_join_at: Timestamp::parse("2026-10-24T00:00:00.000Z").unwrap(),
                    join_seq: Sequence::new(1).unwrap(),
                    leave_seq: None,
                },
                presence: Phase::Disconnected,
                connected: false,
                incarnation: None,
                unread_count: 0,
            })
            .collect(),
            next_cursor: None,
            scan_cursor: head,
            head,
        })
    }
    fn project_id(&self) -> Uuid {
        self.project
    }
    async fn head(&self) -> Result<Checkpoint> {
        Ok(self.current())
    }
    async fn channels(
        &self,
        condition: ChannelListCondition,
        page: PageRequest,
    ) -> Result<ChannelPage> {
        let _rows = self.rows.lock().unwrap();
        let head = self.current().seq;
        let as_of = Timestamp::parse("2026-10-24T00:00:00.000Z").unwrap();
        let mut candidates = vec![self.channel.clone()];
        candidates.extend(self.channels.lock().unwrap().iter().cloned());
        candidates.retain(|row| row.creation_seq.cursor() > page.after);
        if condition == ChannelListCondition::Archived {
            candidates.clear();
        }
        let limit = page.limit.unwrap_or(50);
        let more = candidates.len() > limit;
        candidates.truncate(limit);
        let last = candidates.last().map(|row| row.creation_seq.cursor());
        Ok(ChannelPage {
            page: Page {
                items: candidates
                    .into_iter()
                    .map(|channel| ChannelView {
                        mandatory: channel.name == "dev",
                        channel,
                        last_conversation_at: as_of,
                        archive_at: as_of.after_seconds(2_592_000).unwrap(),
                        archived: false,
                    })
                    .collect(),
                next_cursor: last.filter(|_| more),
                scan_cursor: if more {
                    last.unwrap()
                } else {
                    head.max(page.after)
                },
                head,
            },
            as_of,
        })
    }
    async fn roots(&self, request: requests::RootWindow) -> Result<MessageWindow> {
        self.window(&request.channel, None, request.window)
    }
    async fn pinned(&self, request: requests::PinnedRoot) -> Result<PinnedRoot> {
        let rows = self.rows.lock().unwrap();
        let snapshot = request.snapshot.unwrap_or_else(|| self.current());
        self.validate(snapshot)?;
        let root = rows
            .iter()
            .find(|(_, raw)| {
                raw.message.message.id == request.root_message_id
                    && raw.message.message.root_message_id.is_none()
                    && raw.message.channel == request.channel
                    && raw.message.message.creation_seq.cursor() <= snapshot.seq
            })
            .map(|(_, raw)| self.projected(raw, &rows, snapshot.seq))
            .ok_or_else(|| Error::new(ErrorCode::NotFound, "No fixture root."))?;
        Ok(PinnedRoot { snapshot, root })
    }
    async fn replies(&self, request: requests::ReplyWindow) -> Result<MessageWindow> {
        self.window(
            &request.channel,
            Some(request.root_message_id),
            request.window,
        )
    }
    async fn changes(&self, request: ChangeRequest) -> Result<ChangeBatch> {
        self.batch(request)
    }
    async fn watch(&self, after: Checkpoint) -> Result<Box<dyn ChangeObserver>> {
        if self.fail_setup.load(Ordering::SeqCst) {
            return Err(Error::new(ErrorCode::Busy, "Fixture setup failed."));
        }
        let hints = self.hints.subscribe();
        self.validate(after)?;
        self.starts.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Observer {
            state: self.0.clone(),
            cursor: after,
            through: None,
            hints,
        }))
    }
}
struct Observer {
    state: Arc<Fixture>,
    cursor: Checkpoint,
    through: Option<Checkpoint>,
    hints: watch::Receiver<usize>,
}
#[async_trait]
impl ChangeObserver for Observer {
    fn checkpoint(&self) -> Checkpoint {
        self.cursor
    }
    async fn next(&mut self) -> Result<ChangeBatch> {
        loop {
            self.hints.borrow_and_update();
            self.state.validate(self.cursor)?;
            if self.state.fail_source.load(Ordering::SeqCst) {
                return Err(Error::new(
                    ErrorCode::StorageError,
                    "Fixture source failed.",
                ));
            }
            if self.cursor.seq < self.state.current().seq {
                let batch = self.state.batch(ChangeRequest {
                    after: self.cursor,
                    through: self.through,
                    limit: None,
                })?;
                self.cursor = batch.scan;
                self.through = batch.has_more.then_some(batch.head);
                return Ok(batch);
            }
            self.hints
                .changed()
                .await
                .map_err(|_| Error::new(ErrorCode::IoError, "Fixture closed."))?;
        }
    }
}
impl Drop for Observer {
    fn drop(&mut self) {
        self.state.drops.fetch_add(1, Ordering::SeqCst);
    }
}

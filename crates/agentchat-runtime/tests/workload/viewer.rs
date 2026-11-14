//! Independent complete-body audit for the context-free viewer observer under load.
use agentchat_core::{
    model::Message,
    ports::Storage,
    read::{Checkpoint, MAX_PAGE_BYTES, MessageFilter, ViewerChangeKind},
    sequence::Cursor,
};
use agentchat_protocol::rpc::ChangeFollower;
use agentchat_store::Store;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};
use tokio::sync::watch;

pub struct Observed {
    checkpoint: Checkpoint,
    messages: usize,
    channels: usize,
    digest: String,
    max_items: usize,
    max_bytes: usize,
}
fn hash(digest: &mut Sha256, message: &Message) {
    digest.update(serde_json::to_vec(message).unwrap());
    digest.update(b"\n");
}
fn finish(digest: Sha256) -> String {
    digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub async fn observe(
    mut follower: ChangeFollower,
    progress: Arc<AtomicI64>,
    mut stop: watch::Receiver<bool>,
) -> Observed {
    let mut checkpoint = follower.checkpoint();
    let mut digest = Sha256::new();
    let (mut messages, mut channels, mut max_items, mut max_bytes) = (0, 0, 0, 0);
    loop {
        tokio::select! {
            changed=stop.changed()=>if changed.is_err()||*stop.borrow(){break;},
            batch=follower.next()=>{
                let batch=batch.unwrap();
                assert_eq!(batch.after,checkpoint);
                let bytes=serde_json::to_vec(&batch).unwrap().len();
                assert!(batch.items.len()<=200&&bytes<=MAX_PAGE_BYTES);
                max_items=max_items.max(batch.items.len());max_bytes=max_bytes.max(bytes);
                let mut previous=checkpoint.seq.get();
                for item in batch.items {
                    assert!(item.seq.get()>previous&&item.seq.get()<=batch.scan.seq.get());
                    previous=item.seq.get();
                    match item.change {
                        ViewerChangeKind::ChannelCreated{..}=>channels+=1,
                        ViewerChangeKind::MessageCreated{message}=>{
                            let wire = serde_json::to_value(&message.message).unwrap();
                            assert!(wire.get("read_at").is_none() && wire.get("unread").is_none());
                            assert_eq!(message.reply_count,0);
                            assert_eq!(message.message.thread_id,message.message.message.id);
                            hash(&mut digest,&message.message.message);messages+=1;
                        }
                    }
                }
                checkpoint=batch.scan;
                progress.store(checkpoint.seq.get(),Ordering::SeqCst);
            }
        }
    }
    Observed {
        checkpoint,
        messages,
        channels,
        digest: finish(digest),
        max_items,
        max_bytes,
    }
}

/// Compare the streamed raw-message digest with actual retained rows after reopen.
pub async fn retained(store: &Store, observed: Observed, head: Checkpoint) -> Value {
    assert_eq!(observed.checkpoint, head);
    let mut snapshot = store.snapshot().await.unwrap();
    let stats = snapshot.statistics().await.unwrap();
    assert_eq!(observed.messages as u64, stats.messages);
    assert_eq!(observed.channels as u64, stats.channels);
    let mut cursor = Cursor::START;
    let mut digest = Sha256::new();
    let mut count = 0;
    loop {
        let rows = snapshot
            .messages(&MessageFilter::default(), cursor, 200)
            .await
            .unwrap();
        if rows.is_empty() {
            break;
        }
        for message in rows {
            hash(&mut digest, &message);
            cursor = message.creation_seq.cursor();
            count += 1;
        }
    }
    assert_eq!(count, observed.messages);
    assert_eq!(finish(digest), observed.digest);
    json!({"scope":"typed context-free RPC observation, not rendered HTTP/browser load",
        "checkpoint":head,"messages":count,"channels":observed.channels,
        "complete_body_digest_sha256":observed.digest,
        "maximum_batch_items":observed.max_items,"maximum_batch_bytes":observed.max_bytes})
}

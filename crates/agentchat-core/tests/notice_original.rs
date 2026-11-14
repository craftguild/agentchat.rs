//! Current notice budget and refusal of older receipt-policy headers.
use agentchat_core::{
    delivery::{MAX_WAKE_BYTES, NoticeInput},
    model::{Dispatch, DispatchKind, DispatchNotice, NoticeKind},
    project::InputReceiptPolicy,
    sequence, time,
};
use uuid::Uuid;

#[path = "unit/notice_original.rs"]
mod original_tests;

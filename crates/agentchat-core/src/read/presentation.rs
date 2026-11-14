//! Safe presentation-budget cuts shared by adapters; retrieval policy stays here.

use super::{ChangeBatch, Checkpoint, MessageWindow};
use crate::{Error, ErrorCode, Result};

impl MessageWindow {
    /// Retain the newest `count` complete records after presentation/render
    /// budgeting. Omitted older records remain reachable through `next_before`.
    /// A nonempty page cannot be cut to zero, which would make no progress.
    pub fn retain_latest(&mut self, count: usize) -> Result<()> {
        if count >= self.items.len() {
            return Ok(());
        }
        if count == 0 {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "A nonempty window must retain at least one complete record.",
            ));
        }
        if self.items.windows(2).any(|rows| {
            rows[0].message.message.creation_seq >= rows[1].message.message.creation_seq
        }) || self
            .items
            .iter()
            .any(|m| m.message.message.creation_seq.cursor() > self.snapshot.seq)
        {
            return Err(integrity("Cannot shorten an invalid ordered window."));
        }
        self.items = self.items.split_off(self.items.len() - count);
        self.next_before = Some(self.items[0].message.message.creation_seq.cursor());
        Ok(())
    }
}

impl ChangeBatch {
    /// Split a complete matching-item prefix for a smaller encoded/render
    /// budget. The remainder retains the original scanned tail and fixed head;
    /// no adapter has to invent progress past data it did not return.
    pub fn split_prefix(mut self, count: usize) -> Result<(Self, Option<Self>)> {
        if count >= self.items.len() {
            return Ok((self, None));
        }
        if count == 0 {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "A nonempty change prefix must retain a complete item.",
            ));
        }
        if self.after.project_id != self.scan.project_id
            || self.after.project_id != self.head.project_id
            || self.scan.seq < self.after.seq
            || self.scan.seq > self.head.seq
            || self.has_more != (self.scan.seq < self.head.seq)
        {
            return Err(integrity("Cannot split an invalid change range."));
        }
        let mut previous = self.after.seq;
        for item in &self.items {
            if item.seq.cursor() <= previous || item.seq.cursor() > self.scan.seq {
                return Err(integrity("Cannot split unordered change items."));
            }
            previous = item.seq.cursor();
        }
        let rest = self.items.split_off(count);
        let last = self.items.last().expect("nonempty bounded prefix");
        let boundary = Checkpoint {
            project_id: self.after.project_id,
            seq: last.seq.cursor(),
            event_id: Some(last.event_id),
        };
        let remainder = Self {
            after: boundary,
            scan: self.scan,
            head: self.head,
            items: rest,
            has_more: self.has_more,
        };
        self.scan = boundary;
        self.has_more = true;
        Ok((self, Some(remainder)))
    }
}

fn integrity(message: &str) -> Error {
    Error::new(ErrorCode::IntegrityError, message)
}

//! Serialized page budgets and progress without skipping an unreturned record.

use serde::{Deserialize, Serialize};

use crate::{Error, ErrorCode, Result, sequence::Cursor, validation};

/// Maximum serialized page size, below the transport's 1 MiB envelope limit.
pub const MAX_PAGE_BYTES: usize = 768 * 1024;

/// Common page input. Validation happens at the application boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PageRequest {
    /// Requested maximum; omission means 50, maximum 200.
    pub limit: Option<usize>,
    /// Last consumed ordering key; zero precedes the collection.
    #[serde(default)]
    pub after: Cursor,
}

impl PageRequest {
    /// Check the finite item limit before starting a snapshot.
    pub fn limit(self) -> Result<usize> {
        validation::page_limit(self.limit)
    }
}

/// Finite snapshot page. Reading it has no delivery or receipt side effect.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Page<T> {
    /// Complete records fitting both the item and serialized byte budgets.
    pub items: Vec<T>,
    /// Present when another matching record remains.
    pub next_cursor: Option<Cursor>,
    /// Safe scan progress, including intervals with no matching records.
    pub scan_cursor: Cursor,
    /// Actual committed head captured for this snapshot.
    pub head: Cursor,
}

pub(crate) struct PageBuilder<T> {
    page: Page<T>,
    limit: usize,
    bytes: usize,
    after: Cursor,
}

impl<T: Serialize> PageBuilder<T> {
    pub fn new(input: PageRequest, head: Cursor) -> Result<Self> {
        Ok(Self {
            page: Page {
                items: Vec::new(),
                next_cursor: None,
                scan_cursor: input.after,
                head,
            },
            limit: input.limit()?,
            bytes: 256,
            after: input.after,
        })
    }

    pub fn push(&mut self, key: Cursor, item: T) -> Result<bool> {
        if key <= self.page.scan_cursor || key > self.page.head {
            return Err(Error::new(
                ErrorCode::IntegrityError,
                "Snapshot records are outside their ordered cursor range.",
            ));
        }
        let size = serde_json::to_vec(&item)
            .map_err(|_| serialization())?
            .len()
            + 1;
        if size + 256 > MAX_PAGE_BYTES {
            return Err(Error::new(
                ErrorCode::IntegrityError,
                "A stored record exceeds the finite page budget.",
            ));
        }
        if self.page.items.len() == self.limit || self.bytes + size > MAX_PAGE_BYTES {
            self.page.next_cursor = Some(self.page.scan_cursor);
            return Ok(false);
        }
        self.bytes += size;
        self.page.scan_cursor = key;
        self.page.items.push(item);
        Ok(true)
    }

    pub fn finish(mut self) -> Page<T> {
        if self.page.next_cursor.is_none() {
            self.page.scan_cursor = self.page.head.max(self.after);
        }
        self.page
    }
}

pub(crate) fn serialization() -> Error {
    Error::new(
        ErrorCode::IntegrityError,
        "Query result serialization failed.",
    )
}

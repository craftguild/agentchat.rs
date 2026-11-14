//! Lossless decimal-string cursors for signed-64-bit project event ordering.

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};

use crate::{Error, ErrorCode, Result};

/// A cursor is the last scanned or emitted event sequence; zero precedes history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Cursor(i64);

impl Cursor {
    /// Before the first event.
    pub const START: Self = Self(0);

    /// Construct a nonnegative cursor without accepting lossy numeric values.
    pub fn new(value: i64) -> Result<Self> {
        if value < 0 {
            return Err(invalid());
        }
        Ok(Self(value))
    }

    /// Parse a decimal string, rejecting negative/overflowed or ambiguous input.
    pub fn parse(input: &str) -> Result<Self> {
        if input.is_empty() || !input.bytes().all(|c| c.is_ascii_digit()) {
            return Err(invalid());
        }
        Self::new(input.parse().map_err(|_| invalid())?)
    }

    /// Value for relational ordering and query predicates.
    pub const fn get(self) -> i64 {
        self.0
    }
}

impl Serialize for Cursor {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for Cursor {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        Self::parse(&String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// A committed event sequence is positive; timestamps never define ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Sequence(Cursor);

impl Sequence {
    /// Validate an allocated project event sequence.
    pub fn new(value: i64) -> Result<Self> {
        if value == 0 {
            return Err(invalid());
        }
        Ok(Self(Cursor::new(value)?))
    }

    /// Value for SQLite and cursor ordering.
    pub const fn get(self) -> i64 {
        self.0.get()
    }

    /// Cursor immediately after presenting this event.
    pub const fn cursor(self) -> Cursor {
        self.0
    }
}

impl Serialize for Sequence {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Sequence {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        Self::new(Cursor::deserialize(deserializer)?.get()).map_err(D::Error::custom)
    }
}

fn invalid() -> Error {
    Error::new(
        ErrorCode::InvalidArgument,
        "Expected a decimal-string signed-64-bit event cursor.",
    )
}

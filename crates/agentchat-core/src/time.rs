//! Canonical millisecond UTC timestamps and injectable application clocks.

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};

use crate::{Error, ErrorCode, Result};

/// Canonical, comparable persisted UTC time with millisecond precision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(DateTime<Utc>);

impl Timestamp {
    /// Normalize a clock observation to the public/storage timestamp precision.
    pub fn new(time: DateTime<Utc>) -> Self {
        Self(
            DateTime::from_timestamp_millis(time.timestamp_millis())
                .expect("chrono time fits milliseconds"),
        )
    }

    /// Parse a canonical persisted RFC 3339 timestamp.
    pub fn parse(value: &str) -> Result<Self> {
        let time = DateTime::parse_from_rfc3339(value).map_err(|_| invalid())?;
        let time = Self::new(time.with_timezone(&Utc));
        if time.to_string() != value {
            return Err(invalid());
        }
        Ok(time)
    }

    /// UTC clock value for deadline arithmetic.
    pub const fn datetime(self) -> DateTime<Utc> {
        self.0
    }

    /// Checked deadline offset; invalid clock ranges fail rather than wrapping.
    pub fn after_seconds(self, seconds: i64) -> Result<Self> {
        let duration = chrono::Duration::try_seconds(seconds).ok_or_else(invalid)?;
        self.0
            .checked_add_signed(duration)
            .map(Self::new)
            .ok_or_else(invalid)
    }
}

impl std::fmt::Display for Timestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.to_rfc3339_opts(SecondsFormat::Millis, true))
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        Self::parse(&String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// Time source injected into operations and lease/delivery scheduling.
pub trait Clock: Send + Sync {
    /// Current time; ordering of committed events still uses sequence numbers.
    fn now(&self) -> Timestamp;
}

/// The ordinary production UTC wall clock.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::new(Utc::now())
    }
}

fn invalid() -> Error {
    Error::new(
        ErrorCode::InvalidArgument,
        "Invalid canonical UTC timestamp or deadline.",
    )
}

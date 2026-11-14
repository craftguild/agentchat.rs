//! Shared input validation independent of transport and persistence.

use crate::{Error, ErrorCode, Result};

/// Maximum UTF-8 message-body size.
pub const MAX_BODY_BYTES: usize = 65_536;
/// Maximum search query or reservation-reason size.
pub const MAX_QUERY_BYTES: usize = 1_024;
/// Maximum number of IDs in an atomic receipt or acknowledgement request.
pub const MAX_BULK_IDS: usize = 200;

/// Normalize a handle, rejecting reserved system names and malformed slugs.
pub fn handle(input: &str) -> Result<String> {
    let name = slug(input, 32)?;
    if ["agentchat", "all", "channel", "here", "system"].contains(&name.as_str()) {
        return Err(Error::new(
            ErrorCode::InvalidName,
            "This handle is reserved.",
        ));
    }
    Ok(name)
}

/// Normalize a channel slug using the 64-byte channel limit.
pub fn channel(input: &str) -> Result<String> {
    slug(input, 64)
}

fn slug(input: &str, maximum: usize) -> Result<String> {
    let bytes = input.as_bytes();
    let valid = !bytes.is_empty()
        && bytes.len() <= maximum
        && bytes[0].is_ascii_alphabetic()
        && !input.ends_with('-')
        && !input.contains("--")
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-');
    if !valid {
        return Err(Error::new(
            ErrorCode::InvalidName,
            "Expected an ASCII slug beginning with a letter.",
        ));
    }
    Ok(input.to_ascii_lowercase())
}

/// Validate a body before entering the mutation transaction.
pub fn body(input: &str) -> Result<()> {
    if input.trim().is_empty() || input.len() > MAX_BODY_BYTES {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "A body must be nonempty and at most 65536 UTF-8 bytes.",
        ));
    }
    Ok(())
}

/// Validate TTL seconds, applying the default when omitted.
pub fn ttl(input: Option<u32>) -> Result<u32> {
    let ttl = input.unwrap_or(600);
    if !(30..=3_600).contains(&ttl) {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "Lease TTL must be 30 through 3600 seconds.",
        ));
    }
    Ok(ttl)
}

/// Validate a requested finite page size.
pub fn page_limit(input: Option<usize>) -> Result<usize> {
    let limit = input.unwrap_or(50);
    if !(1..=200).contains(&limit) {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "Page limit must be 1 through 200.",
        ));
    }
    Ok(limit)
}

/// Validate a nonempty atomic ID set's cardinality before any mutation.
pub fn bulk_len(len: usize) -> Result<()> {
    if !(1..=MAX_BULK_IDS).contains(&len) {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "Supply 1 through 200 IDs.",
        ));
    }
    Ok(())
}

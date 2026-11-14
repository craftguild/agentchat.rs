//! CLI duration syntax only; semantic limits belong to the shared application.
use agentchat_core::{Error, ErrorCode, Result};

pub(crate) fn seconds(input: Option<&str>) -> Result<Option<u32>> {
    let Some(input) = input else { return Ok(None) };
    let (digits, multiplier) = match input.as_bytes().last() {
        Some(b's') => (&input[..input.len() - 1], 1),
        Some(b'm') => (&input[..input.len() - 1], 60),
        Some(b'h') => (&input[..input.len() - 1], 3600),
        Some(b'd') => (&input[..input.len() - 1], 86400),
        _ => (input, 1),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    let value = digits
        .parse::<u32>()
        .map_err(|_| invalid())?
        .checked_mul(multiplier)
        .ok_or_else(invalid)?;
    Ok(Some(value))
}
fn invalid() -> Error {
    Error::new(
        ErrorCode::InvalidArgument,
        "Delivery confirmation timeout requires whole seconds or s/m/h/d, for example 5m.",
    )
}

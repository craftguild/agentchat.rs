//! Bounded literal UTF-8 body input, shared by explicit and implicit stdin.

use crate::args::Body;
use agentchat_core::{Error, ErrorCode, Result};
use std::io::IsTerminal;
use tokio::io::{AsyncRead, AsyncReadExt};

pub(crate) async fn body(input: Body) -> Result<String> {
    read(input, tokio::io::stdin(), std::io::stdin().is_terminal()).await
}
pub(crate) async fn read(
    input: Body,
    reader: impl AsyncRead + Unpin,
    terminal: bool,
) -> Result<String> {
    if let Some(body) = input.body {
        if input.stdin {
            return Err(invalid("Choose one message body source."));
        }
        if body.len() > 65_536 {
            return Err(invalid("Message body exceeds 65,536 UTF-8 bytes."));
        }
        return Ok(body);
    }
    if terminal && !input.stdin {
        return Err(invalid("Supply --body or --stdin for message text."));
    }
    let mut bytes = Vec::new();
    reader
        .take(65_537)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| Error::new(ErrorCode::IoError, "Message stdin could not be read."))?;
    if bytes.len() > 65_536 {
        return Err(invalid("Message body exceeds 65,536 UTF-8 bytes."));
    }
    String::from_utf8(bytes).map_err(|_| invalid("Message stdin must contain valid UTF-8."))
}
fn invalid(message: &str) -> Error {
    Error::new(ErrorCode::InvalidArgument, message)
}

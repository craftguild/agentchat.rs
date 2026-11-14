//! Passive WebSocket framing only: bytes, masks and control frames stay native.
use super::{json_stream::Feed, utf8::Utf8};
use tokio_tungstenite::tungstenite::Message;
pub(super) enum Packet {
    Metadata(Message),
    Unobservable,
    Ignored,
}
pub(super) struct Tap {
    frontend: bool,
    http: bool,
    http_tail: u32,
    status: Vec<u8>,
    opaque: bool,
    header: Vec<u8>,
    remaining: u64,
    mask: [u8; 4],
    masked: bool,
    offset: u64,
    data: bool,
    fin: bool,
    active: bool,
    invalid: bool,
    ignored: bool,
    feed: Option<Feed>,
    utf8: Utf8,
}
impl Tap {
    pub(super) fn new(frontend: bool) -> Self {
        Self {
            frontend,
            http: true,
            http_tail: 0,
            status: Vec::new(),
            opaque: false,
            header: Vec::with_capacity(14),
            remaining: 0,
            mask: [0; 4],
            masked: false,
            offset: 0,
            data: false,
            fin: false,
            active: false,
            invalid: false,
            ignored: false,
            feed: None,
            utf8: Utf8::default(),
        }
    }
    /// A true result identifies a final message byte still held by the relay.
    /// Fencing it precedes server entry without buffering the complete message.
    pub(super) fn disable(&mut self) {
        self.feed = None;
        self.opaque = true;
    }
    pub(super) fn byte(&mut self, byte: u8, observe: bool) -> bool {
        if self.opaque {
            return false;
        }
        if self.http {
            if !self.frontend && self.status.len() < 16 {
                self.status.push(byte);
            }
            self.http_tail = (self.http_tail << 8) | u32::from(byte);
            if self.http_tail == 0x0d0a0d0a {
                self.http = false;
                if !self.frontend
                    && !self.status.starts_with(b"HTTP/1.1 101")
                    && !self.status.starts_with(b"HTTP/1.0 101")
                {
                    self.opaque = true;
                }
                self.status.clear();
            }
            return false;
        }
        if self.remaining == 0 {
            self.header.push(byte);
            if self.header.len() < 2 {
                return false;
            }
            let size = self.header[1] & 127;
            let extra = match size {
                126 => 2,
                127 => 8,
                _ => 0,
            };
            let masked = self.header[1] & 128 != 0;
            let length = 2 + extra + if masked { 4 } else { 0 };
            if self.header.len() < length {
                return false;
            }
            let flags = self.header[0];
            let opcode = flags & 15;
            self.fin = flags & 128 != 0;
            self.data = opcode < 8;
            self.masked = masked;
            self.offset = 0;
            self.remaining = match size {
                126 => u64::from(u16::from_be_bytes(
                    self.header[2..4].try_into().expect("frame length"),
                )),
                127 => u64::from_be_bytes(self.header[2..10].try_into().expect("frame length")),
                n => u64::from(n),
            };
            if masked {
                self.mask.copy_from_slice(&self.header[length - 4..]);
            }
            if opcode == 1 || opcode == 2 {
                self.utf8 = Utf8::default();
                self.ignored = !observe;
                self.invalid = self.active || opcode == 2 || flags & 112 != 0;
                self.active = true;
                self.feed = if !self.invalid && observe {
                    Some(Feed::new())
                } else {
                    None
                };
            } else if opcode < 8 && opcode != 0 {
                self.invalid = true;
                self.ignored = !observe;
                self.feed = None;
                self.active = true;
            } else if opcode == 0 && (!self.active || flags & 112 != 0) {
                self.invalid = true;
            }
            if self.data
                && (self.frontend != masked
                    || (size == 126 && self.remaining < 126)
                    || (size == 127 && (self.remaining < 65536 || self.remaining >> 63 != 0)))
            {
                self.invalid = true;
            }
            self.header.clear();
            return self.remaining == 0 && self.data && self.fin;
        }
        if self.data
            && let Some(feed) = &mut self.feed
        {
            let decoded = if self.masked {
                byte ^ self.mask[(self.offset % 4) as usize]
            } else {
                byte
            };
            if !self.utf8.push(decoded) {
                self.invalid = true;
            }
            feed.push(decoded);
        }
        self.offset += 1;
        self.remaining -= 1;
        self.remaining == 0 && self.data && self.fin
    }
    pub(super) async fn flush(&mut self) -> bool {
        if let Some(feed) = &mut self.feed
            && !feed.flush().await
        {
            self.feed = None;
            self.invalid = true;
            return false;
        }
        true
    }
    pub(super) async fn finish(&mut self) -> Packet {
        self.active = false;
        if self.ignored {
            self.feed = None;
            self.invalid = false;
            return Packet::Ignored;
        }
        if self.invalid || !self.utf8.complete() {
            self.feed = None;
            self.invalid = false;
            return Packet::Unobservable;
        }
        match self.feed.take() {
            Some(feed) => match feed.finish().await {
                Some(metadata) => Packet::Metadata(metadata),
                None => Packet::Unobservable,
            },
            None => Packet::Ignored,
        }
    }
}

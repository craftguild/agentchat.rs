//! Streaming selective JSON visitor. Unknown values use IgnoredAny, not trees.
use serde::{
    Deserialize, Deserializer,
    de::{DeserializeSeed, IgnoredAny, MapAccess, Visitor},
};
use serde_json::value::RawValue;
use std::{
    cell::Cell,
    fmt,
    io::{self, Read},
    rc::Rc,
};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
#[derive(Clone)]
struct Budget {
    left: Rc<Cell<Option<usize>>>,
    prefix: Rc<Cell<bool>>,
    scalar: Rc<Cell<bool>>,
    last: Rc<Cell<u8>>,
}
impl Budget {
    fn set(&self, bytes: Option<usize>) {
        self.left.set(bytes.map(|n| n + 1));
        self.prefix.set(bytes.is_some());
        self.scalar.set(false);
    }
}
enum Chunk {
    Bytes(Vec<u8>),
    End,
}
struct Input {
    receiver: mpsc::Receiver<Chunk>,
    bytes: Vec<u8>,
    offset: usize,
    ended: bool,
    budget: Budget,
    depth: usize,
    string: bool,
    escaped: bool,
}
impl Read for Input {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        while self.offset == self.bytes.len() {
            if self.ended {
                return Ok(0);
            }
            match self.receiver.blocking_recv() {
                Some(Chunk::Bytes(bytes)) => {
                    self.bytes = bytes;
                    self.offset = 0;
                }
                Some(Chunk::End) | None => {
                    self.ended = true;
                    return Ok(0);
                }
            }
        }
        if let Some(left) = self.budget.left.get() {
            let byte = self.bytes[self.offset];
            if !(self.budget.prefix.get()
                && (byte.is_ascii_whitespace() || matches!(byte, b':' | b',')))
            {
                if self.budget.prefix.get()
                    && self.budget.scalar.get()
                    && matches!(byte, b'{' | b'[')
                {
                    return Err(io::Error::other("compound lifecycle scalar"));
                }
                self.budget.prefix.set(false);
                if left == 0 {
                    return Err(io::Error::other("lifecycle metadata token bound"));
                }
                self.budget.left.set(Some(left - 1));
            }
        }
        // IgnoredAny stores a structural stack, not values. Bound that stack
        // independently of native payload size before serde can grow it.
        let byte = self.bytes[self.offset];
        if self.string {
            if self.escaped {
                self.escaped = false;
            } else if byte == b'\\' {
                self.escaped = true;
            } else if byte == b'"' {
                self.string = false;
            }
        } else {
            match byte {
                b'"' => self.string = true,
                b'{' | b'[' => {
                    self.depth += 1;
                    if self.depth > 128 {
                        return Err(io::Error::other("lifecycle observation depth"));
                    }
                }
                b'}' | b']' => self.depth = self.depth.saturating_sub(1),
                _ => {}
            }
        }
        // serde_json's IoRead consumes bytes from this bounded cached chunk.
        out[0] = byte;
        self.budget.last.set(byte);
        self.offset += 1;
        Ok(1)
    }
}
/// At most one transit chunk and bounded selected tokens are owned by a worker.
/// Dropping the sender unblocks EOF; no blocking worker survives its connection.
pub(super) struct Feed {
    sender: mpsc::Sender<Chunk>,
    result: oneshot::Receiver<Option<Message>>,
    buffer: Vec<u8>,
}
impl Feed {
    pub(super) fn new() -> Self {
        let (sender, receiver) = mpsc::channel(1);
        let (done, result) = oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let budget = Budget {
                left: Rc::new(Cell::new(None)),
                prefix: Rc::new(Cell::new(false)),
                scalar: Rc::new(Cell::new(false)),
                last: Rc::new(Cell::new(0)),
            };
            let input = Input {
                receiver,
                bytes: Vec::new(),
                offset: 0,
                ended: false,
                budget: budget.clone(),
                depth: 0,
                string: false,
                escaped: false,
            };
            let mut json = serde_json::Deserializer::from_reader(input);
            let parsed = Seed {
                context: Context::Top,
                budget,
            }
            .deserialize(&mut json);
            let result = parsed
                .ok()
                .filter(|_| json.end().is_ok())
                .map(|fields| Message::Text(fields.json().into()));
            let _ = done.send(result);
        });
        Self {
            sender,
            result,
            buffer: Vec::with_capacity(8192),
        }
    }
    pub(super) fn push(&mut self, byte: u8) {
        self.buffer.push(byte);
    }
    pub(super) async fn flush(&mut self) -> bool {
        if self.buffer.is_empty() {
            return true;
        }
        self.sender
            .send(Chunk::Bytes(std::mem::replace(
                &mut self.buffer,
                Vec::with_capacity(8192),
            )))
            .await
            .is_ok()
    }
    pub(super) async fn finish(mut self) -> Option<Message> {
        if !self.flush().await || self.sender.send(Chunk::End).await.is_err() {
            return None;
        }
        self.result.await.ok().flatten()
    }
}
#[derive(Clone, Copy)]
enum Context {
    Top,
    Params,
    Result,
    Thread,
}
#[derive(Deserialize)]
#[serde(field_identifier)]
enum Field {
    #[serde(rename = "id")]
    Id,
    #[serde(rename = "method")]
    Method,
    #[serde(rename = "params")]
    Params,
    #[serde(rename = "result")]
    Result,
    #[serde(rename = "error")]
    Error,
    #[serde(rename = "ephemeral")]
    Ephemeral,
    #[serde(rename = "threadId")]
    ThreadId,
    #[serde(rename = "thread")]
    Thread,
    #[serde(other)]
    Other,
}
#[derive(Default)]
struct Fields {
    id: Option<Box<RawValue>>,
    method: Option<Box<RawValue>>,
    root: Option<Box<RawValue>>,
    ephemeral: Option<Box<RawValue>>,
    params: Option<Box<Fields>>,
    result: Option<Box<Fields>>,
    thread: Option<Box<Fields>>,
    error: Option<bool>,
}
impl Fields {
    fn json(&self) -> String {
        let mut parts = Vec::new();
        for (name, value) in [
            ("id", &self.id),
            ("method", &self.method),
            ("threadId", &self.root),
            ("ephemeral", &self.ephemeral),
        ] {
            if let Some(value) = value {
                parts.push(format!("\"{name}\":{}", value.get()));
            }
        }
        for (name, value) in [
            ("params", &self.params),
            ("result", &self.result),
            ("thread", &self.thread),
        ] {
            if let Some(value) = value {
                parts.push(format!("\"{name}\":{}", value.json()));
            }
        }
        if let Some(error) = self.error {
            parts.push(format!("\"error\":{}", if error { "true" } else { "null" }));
        }
        format!("{{{}}}", parts.join(","))
    }
}
struct Seed {
    context: Context,
    budget: Budget,
}
impl<'de> DeserializeSeed<'de> for Seed {
    type Value = Fields;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Fields, D::Error> {
        // deserialize_option peeks the first nonspace byte before visit_some.
        // Keep that lookahead bounded; nested nonmaps are skipped, not owned.
        self.budget.set(Some(6));
        d.deserialize_option(self)
    }
}
impl<'de> Visitor<'de> for Seed {
    type Value = Fields;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a lifecycle metadata map or null")
    }
    fn visit_none<E: serde::de::Error>(self) -> Result<Fields, E> {
        self.budget.set(None);
        Ok(Fields::default())
    }
    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Fields, D::Error> {
        if self.budget.last.get() == b'{' {
            d.deserialize_map(self)
        } else if matches!(self.context, Context::Top) {
            Err(serde::de::Error::custom("unobservable envelope shape"))
        } else {
            self.budget.set(None);
            IgnoredAny::deserialize(d)?;
            Ok(Fields::default())
        }
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Fields, A::Error> {
        self.budget.set(None);
        let mut fields = Fields::default();
        loop {
            self.budget.set(Some(128));
            let key = map.next_key::<Field>()?;
            self.budget.set(None);
            let Some(key) = key else {
                break;
            };
            let slot = match (self.context, key) {
                (Context::Top, Field::Id) => Some((&mut fields.id, 8192)),
                (Context::Thread, Field::Id) => Some((&mut fields.id, 6146)),
                (Context::Top, Field::Method) => Some((&mut fields.method, 128)),
                (Context::Params | Context::Result, Field::ThreadId) => {
                    Some((&mut fields.root, 6146))
                }
                (Context::Params, Field::Ephemeral) => Some((&mut fields.ephemeral, 6)),
                (Context::Top, Field::Params) => {
                    fields.params = Some(Box::new(map.next_value_seed(Seed {
                        context: Context::Params,
                        budget: self.budget.clone(),
                    })?));
                    None
                }
                (Context::Top, Field::Result) => {
                    fields.result = Some(Box::new(map.next_value_seed(Seed {
                        context: Context::Result,
                        budget: self.budget.clone(),
                    })?));
                    None
                }
                (Context::Result, Field::Thread) => {
                    fields.thread = Some(Box::new(map.next_value_seed(Seed {
                        context: Context::Thread,
                        budget: self.budget.clone(),
                    })?));
                    None
                }
                (Context::Top, Field::Error) => {
                    fields.error = Some(map.next_value_seed(ErrorSeed)?);
                    None
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                    None
                }
            };
            if let Some((slot, limit)) = slot {
                self.budget.set(Some(limit));
                self.budget.scalar.set(true);
                *slot = Some(map.next_value()?);
                self.budget.set(None);
            }
        }
        Ok(fields)
    }
}
struct ErrorSeed;
impl<'de> DeserializeSeed<'de> for ErrorSeed {
    type Value = bool;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<bool, D::Error> {
        d.deserialize_option(self)
    }
}
impl<'de> Visitor<'de> for ErrorSeed {
    type Value = bool;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an ignored error value")
    }
    fn visit_none<E: serde::de::Error>(self) -> Result<bool, E> {
        Ok(false)
    }
    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<bool, D::Error> {
        IgnoredAny::deserialize(d)?;
        Ok(true)
    }
}

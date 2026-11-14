//! Durable write intent admits only exact prefixes of an owned counter update.
use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Pending {
    pub from: u64,
    pub to: u64,
}
fn number(bytes: &[u8]) -> Option<u64> {
    let value = std::str::from_utf8(bytes)
        .ok()?
        .strip_suffix('\n')?
        .parse::<u64>()
        .ok()?;
    (value <= MAX && bytes == format!("{value}\n").as_bytes()).then_some(value)
}
fn prefix(bytes: &[u8], from: u64, to: u64) -> bool {
    let old = format!("{from}\n").into_bytes();
    let next = format!("{to}\n").into_bytes();
    (0..=next.len()).any(|end| {
        let mut cut = old.clone();
        cut.resize(cut.len().max(end), 0);
        cut[..end].copy_from_slice(&next[..end]);
        cut == bytes
    })
}
pub(super) fn validate(anchor: &Anchor) -> Result<()> {
    if anchor.floor > MAX
        || anchor
            .pending
            .as_ref()
            .is_some_and(|p| p.from >= p.to || p.to != anchor.floor || p.to > MAX)
    {
        return Err(required());
    }
    Ok(())
}
pub(super) fn observe(anchor: &Anchor, bytes: &[u8]) -> Result<()> {
    if let Some(pending) = &anchor.pending {
        if prefix(bytes, pending.from, pending.to) {
            return Ok(());
        }
    } else if number(bytes).is_some_and(|count| count <= anchor.floor) {
        return Ok(());
    }
    Err(required())
}
pub(super) fn upgrade(mut anchor: Anchor, raw: &[u8]) -> Result<Anchor> {
    if anchor.version != 1 || anchor.pending.is_some() || anchor.floor > MAX {
        return Err(required());
    }
    let from = if let Some(count) = number(raw) {
        anchor.floor = anchor.floor.max(count);
        count
    } else if anchor.floor > 0 && prefix(raw, anchor.floor - 1, anchor.floor) {
        // v1 syncs its floor before overwriting the old decimal counter. Only
        // this exact one-step legacy write prefix may migrate as incomplete.
        anchor.floor - 1
    } else {
        return Err(required());
    };
    anchor.version = 2;
    anchor.pending = (from < anchor.floor).then_some(Pending {
        from,
        to: anchor.floor,
    });
    Ok(anchor)
}
impl Locked {
    pub(super) fn recover(&mut self) -> Result<()> {
        self.check()?;
        validate(&self.anchor)?;
        let raw = bytes(&mut self.file)?;
        if let Some(pending) = &self.anchor.pending {
            if !prefix(&raw, pending.from, pending.to) {
                return Err(required());
            }
            return self.complete();
        }
        let count = number(&raw).ok_or_else(required)?;
        if count > self.anchor.floor {
            return Err(required());
        }
        self.count = count;
        if count < self.anchor.floor {
            self.reserve(self.anchor.floor)?;
            self.complete()?;
        }
        Ok(())
    }
    pub(super) fn reserve(&mut self, next: u64) -> Result<()> {
        if self.anchor.pending.is_some() || next <= self.count || next > MAX {
            return Err(required());
        }
        let mut anchor = self.anchor.clone();
        anchor.floor = next;
        anchor.pending = Some(Pending {
            from: self.count,
            to: next,
        });
        self.persist(anchor)
    }
    pub(super) fn complete(&mut self) -> Result<()> {
        self.check()?;
        let pending = self.anchor.pending.clone().ok_or_else(required)?;
        if !prefix(&bytes(&mut self.file)?, pending.from, pending.to) {
            return Err(required());
        }
        self.file.seek(SeekFrom::Start(0)).map_err(|_| required())?;
        let bytes = format!("{}\n", pending.to);
        self.file
            .write_all(bytes.as_bytes())
            .map_err(|_| required())?;
        self.file
            .set_len(bytes.len() as u64)
            .and_then(|_| self.file.sync_all())
            .map_err(|_| required())?;
        self.check()?;
        let mut anchor = self.anchor.clone();
        anchor.pending = None;
        self.persist(anchor)?;
        self.count = pending.to;
        Ok(())
    }
}

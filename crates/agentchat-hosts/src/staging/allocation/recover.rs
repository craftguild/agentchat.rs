//! Original directory/socket cleanup shares the finite pending-origin budget.
use super::*;
use crate::{
    claude::process::BirthState,
    staging::{OriginReport, inactive},
};

fn pending(report: &mut OriginReport) {
    report.preserved += 1;
}
fn retire(
    index: &Directory,
    name: &OsStr,
    snapshot: &Snapshot,
    report: &mut OriginReport,
) -> Result<()> {
    if index.remove_record(name, snapshot)? {
        report.removed += 1;
    }
    Ok(())
}
pub(in crate::staging) fn recover(
    index: &Directory,
    name: &OsStr,
    snapshot: &Snapshot,
    value: &Record,
    repair: bool,
    report: &mut OriginReport,
) -> Result<()> {
    let facts = value.intent.allocation.as_ref().ok_or_else(unavailable)?;
    let parent = Directory::shared(&record::path(&value.intent.parent.path))?;
    if parent.id != value.intent.parent.id {
        return Err(unavailable());
    }
    let path = parent.path.join(record::component(&value.intent.stage)?);
    let root = root(facts)?;
    let guard = facts
        .guard
        .as_ref()
        .map(|guard| {
            let name = record::component(&guard.name)?;
            let current = root.read(name)?.ok_or_else(unavailable)?;
            if current.id != guard.id {
                return Err(unavailable());
            }
            Ok((name, current))
        })
        .transpose()?;
    if !repair {
        pending(report);
        return Ok(());
    }
    let canonical_name = record::component(&value.name)?;
    if name != canonical_name
        && let Some(current) = index.read(canonical_name)?
    {
        let current = Record::decode(index, canonical_name, &current)?;
        let new = current.intent.allocation.as_ref().ok_or_else(unavailable)?;
        let mut old_plan = value.intent.clone();
        old_plan.allocation = Some(new.clone());
        if current.creator == value.creator
            && current.intent == old_plan
            && new.extends(facts)
            && (!value.retiring || current.retiring)
        {
            return retire(index, name, snapshot, report);
        }
    }
    let endpoint_name = record::component(&value.intent.target)?;
    let endpoint = root.read(endpoint_name)?;
    let claim = endpoint
        .as_ref()
        .map(|s| published::read(&root, value, s))
        .transpose()?;
    let own = claim
        .as_ref()
        .map(|c| published::belongs(value, c))
        .transpose()?
        .unwrap_or(false);
    if own
        && facts.endpoint.as_ref().is_some_and(|p| {
            endpoint
                .as_ref()
                .is_none_or(|s| *p != FileProof::snapshot(s))
        })
    {
        return Err(unavailable());
    }
    let exists = !Directory::absent(&path)?;
    if !exists && (facts.directory.is_none() || value.retiring) && !own {
        // A completed release/empty intent no longer owns external objects.
        // A live successor's lifetime lock must not pin this old index entry.
        if name != canonical_name && index.read(canonical_name)?.is_some() {
            report.removed += writer::checkpoint(index, name, snapshot, value)?;
            pending(report);
            return Ok(());
        }
        parent.sync()?;
        return retire(index, name, snapshot, report);
    }
    let successor = endpoint.is_some() && !own && facts.native.is_some();
    // A proved different publication owns a different directory. Observe its
    // original guard identity, but never take its active lifetime lock or FD.
    let _guard = if !successor {
        guard
            .as_ref()
            .map(|(name, snapshot)| root.lock_record(name, snapshot))
            .transpose()?
    } else {
        None
    };
    let _endpoint_lock = if own {
        endpoint
            .as_ref()
            .map(|s| root.lock_record(endpoint_name, s))
            .transpose()?
    } else {
        None
    };
    let current = || -> Result<()> {
        parent.current()?;
        root.current()?;
        if let Some((name, snapshot)) = &guard
            && !root.matches(name, snapshot)?
        {
            return Err(unavailable());
        }
        if value.creator.birth_state() != BirthState::Gone || root.read(endpoint_name)? != endpoint
        {
            return Err(unavailable());
        }
        Ok(())
    };
    current()?;
    if facts.directory.is_none() {
        if exists {
            return Err(unavailable());
        }
        parent.sync()?;
        return retire(index, name, snapshot, report);
    }
    if !exists {
        if !value.retiring {
            return Err(unavailable());
        }
        if name != canonical_name && index.read(canonical_name)?.is_some() {
            report.removed += writer::checkpoint(index, name, snapshot, value)?;
            pending(report);
            return Ok(());
        }
        if own {
            let endpoint = endpoint.as_ref().ok_or_else(unavailable)?;
            if root.remove_record(endpoint_name, endpoint)? {
                report.removed += 1;
            }
        }
        parent.sync()?;
        return retire(index, name, snapshot, report);
    }
    if report.directories_removed != 0 || report.removed + facts.sockets.len() + 2 > 16 {
        pending(report);
        return Ok(());
    }
    let directory = Directory::open(&path)?;
    if Some(directory.id) != facts.directory || directory.parent_id()? != parent.id {
        return Err(unavailable());
    }
    // Validate every child before removing the first. Missing children are
    // expected only after the durable retirement intent, never silently adopted.
    for entry in std::fs::read_dir(&path).map_err(|_| unavailable())?.take(3) {
        let name = entry.map_err(|_| unavailable())?.file_name();
        let Some((_, id)) = facts.sockets.iter().find(|(s, _)| name == s.as_str()) else {
            return Err(unavailable());
        };
        if directory.socket(&name)?.as_ref() != Some(id) {
            return Err(unavailable());
        }
    }
    for (name, id) in &facts.sockets {
        let current = directory.socket(name.as_ref())?;
        if current.as_ref().is_some_and(|now| now != id) || current.is_none() && !value.retiring {
            return Err(unavailable());
        }
        inactive::socket(&path.join(name))?;
    }
    if !value.retiring || name != canonical_name || own && facts.endpoint.is_none() {
        let mut checkpoint = value.clone();
        if own {
            checkpoint
                .intent
                .allocation
                .as_mut()
                .ok_or_else(unavailable)?
                .endpoint = endpoint.as_ref().map(FileProof::snapshot);
        }
        report.removed += writer::checkpoint(index, name, snapshot, &checkpoint)?;
        pending(report);
        return Ok(());
    }
    for (name, id) in &facts.sockets {
        current()?;
        if directory.remove_socket(name.as_ref(), id)? {
            report.removed += 1;
        }
        #[cfg(test)]
        super::tests::cut("socket", &path);
    }
    current()?;
    directory.remove_empty(parent.id)?;
    report.directories_removed += 1;
    #[cfg(test)]
    super::tests::cut("directory", &path);
    if own && root.remove_record(endpoint_name, endpoint.as_ref().ok_or_else(unavailable)?)? {
        report.removed += 1;
    }
    parent.sync()?;
    retire(index, name, snapshot, report)
}

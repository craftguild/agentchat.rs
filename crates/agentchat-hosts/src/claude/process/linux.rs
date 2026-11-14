//! Bounded /proc observations for an explicitly identified same-user process.
use super::birth::Birth;
use super::{Image, ProcessIdentity, Start, unavailable};
use agentchat_core::Result;
use std::{fs::File, io::Read, path::Path};
use uuid::Uuid;

pub(super) fn observe(pid: u32) -> Result<ProcessIdentity> {
    let directory = std::path::PathBuf::from(format!("/proc/{pid}"));
    let boot = boot()?;
    let first = stat(pid, &read(&directory.join("stat"))?)?;
    let uid = uids(&read(&directory.join("status"))?)?;
    let image = image(&directory)?;
    let second = stat(pid, &read(&directory.join("stat"))?)?;
    // A PID may be reused or exec during a multi-file /proc observation. Check
    // both the kernel start identity and executable/UID again, not just a path.
    if first != second
        || uid != uids(&read(&directory.join("status"))?)?
        || image != self::image(&directory)?
    {
        return Err(unavailable());
    }
    Ok(ProcessIdentity {
        pid,
        boot,
        start: Start::Linux { ticks: first },
        uid,
        image,
    })
}
pub(super) fn boot() -> Result<Uuid> {
    Uuid::parse_str(read(Path::new("/proc/sys/kernel/random/boot_id"))?.trim())
        .map_err(|_| unavailable())
}
pub(super) fn birth(pid: u32) -> Result<Birth> {
    let value = read(Path::new(&format!("/proc/{pid}/stat")))?;
    let (ticks, exited) = birth_stat(pid, &value)?;
    if ticks == 0 {
        return Err(unavailable());
    }
    Ok(Birth {
        start: Start::Linux { ticks },
        exited,
    })
}
pub(super) fn continuity(pid: u32) -> Result<super::continuity::Observation> {
    Ok(super::continuity::Observation {
        birth: birth(pid)?,
        uid: uids(&read(Path::new(&format!("/proc/{pid}/status")))?)?,
    })
}
pub(super) fn parent_pid(pid: u32) -> Result<u32> {
    let value = read(Path::new(&format!("/proc/{pid}/stat")))?;
    stat(pid, &value)?;
    let (_, rest) = value.rsplit_once(')').ok_or_else(unavailable)?;
    rest.split_whitespace()
        .nth(1)
        .ok_or_else(unavailable)?
        .parse()
        .map_err(|_| unavailable())
}
fn image(directory: &Path) -> Result<Image> {
    let link = directory.join("exe");
    let path = std::fs::read_link(&link).map_err(|_| unavailable())?;
    // Follow /proc/PID/exe, not a second lookup of a possibly replaced disk path.
    Image::new(&path, std::fs::metadata(link).map_err(|_| unavailable())?)
}
fn read(path: &Path) -> Result<String> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|_| unavailable())?
        .take(65_537)
        .read_to_end(&mut bytes)
        .map_err(|_| unavailable())?;
    if bytes.len() > 65_536 {
        return Err(unavailable());
    }
    String::from_utf8(bytes).map_err(|_| unavailable())
}
fn stat(pid: u32, value: &str) -> Result<u64> {
    let (start, exited) = birth_stat(pid, value)?;
    if exited {
        Err(unavailable())
    } else {
        Ok(start)
    }
}
fn birth_stat(pid: u32, value: &str) -> Result<(u64, bool)> {
    let (prefix, rest) = value.rsplit_once(')').ok_or_else(unavailable)?;
    let actual = prefix.split_once(" (").ok_or_else(unavailable)?.0;
    if actual.parse::<u32>().ok() != Some(pid) {
        return Err(unavailable());
    }
    let fields = rest.split_whitespace().take(20).collect::<Vec<_>>();
    if fields.len() != 20
        // Earlier kernel fields must be numeric; otherwise a shifted start
        // field could be misread as positive PID replacement. The comm is
        // parsed separately and may contain arbitrary spaces or delimiters.
        || fields[1..19].iter().any(|field| {
            field.parse::<i128>().ok().is_none_or(|value| {
                value < i64::MIN as i128 || value > u64::MAX as i128
            })
        })
        || !matches!(
            fields[0],
            "R" | "S" | "D" | "T" | "t" | "K" | "W" | "P" | "I" | "Z" | "X" | "x"
        )
    {
        return Err(unavailable());
    }
    // stat field 22 follows the parenthesized comm (which may contain ')',
    // spaces or newlines). The first remaining field is field 3, process state.
    Ok((
        fields[19].parse().map_err(|_| unavailable())?,
        matches!(fields[0], "Z" | "X" | "x"),
    ))
}
fn uids(value: &str) -> Result<u32> {
    let mut lines = value.lines().filter_map(|line| line.strip_prefix("Uid:"));
    let line = lines.next().ok_or_else(unavailable)?;
    if lines.next().is_some() {
        return Err(unavailable());
    }
    let values = line
        .split_whitespace()
        .map(str::parse::<u32>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| unavailable())?;
    if values.len() != 4 || values.iter().any(|value| *value != values[0]) {
        return Err(unavailable());
    }
    Ok(values[0])
}

#[cfg(test)]
#[path = "../../../tests/process/parsing.rs"]
mod tests;

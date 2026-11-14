//! Safe libproc/sysctl wrappers; no local FFI declarations or unsafe blocks.
use super::birth::Birth;
use super::{Image, ProcessIdentity, Start, unavailable};
use agentchat_core::Result;
use libproc::{
    bsd_info::BSDInfo,
    proc_pid::{pidinfo, pidpath},
};
use std::path::Path;
use sysctl::Sysctl;
use uuid::Uuid;

pub(super) fn observe(pid: u32) -> Result<ProcessIdentity> {
    let boot = boot()?;
    let first = info(pid)?;
    let image = image(pid)?;
    let second = info(pid)?;
    if first != second || image != self::image(pid)? {
        return Err(unavailable());
    }
    Ok(ProcessIdentity {
        pid,
        boot,
        start: first.0,
        uid: first.1,
        image,
    })
}
pub(super) fn boot() -> Result<Uuid> {
    let boot = sysctl::Ctl::new("kern.bootsessionuuid")
        .and_then(|ctl| ctl.value_string())
        .map_err(|_| unavailable())?;
    Uuid::parse_str(boot.trim()).map_err(|_| unavailable())
}
pub(super) fn birth(pid: u32) -> Result<Birth> {
    let bsd = pidinfo::<BSDInfo>(pid as i32, 0).map_err(|_| unavailable())?;
    // Birth retention deliberately ignores reversible UID/image changes. The
    // Initial capture enforces both; ongoing continuity separately checks UID.
    if bsd.pbi_pid != pid
        || !matches!(bsd.pbi_status, 2..=5)
        || bsd.pbi_start_tvsec == 0
        || bsd.pbi_start_tvusec >= 1_000_000
    {
        return Err(unavailable());
    }
    Ok(Birth {
        start: Start::Macos {
            seconds: bsd.pbi_start_tvsec,
            microseconds: bsd.pbi_start_tvusec,
        },
        exited: bsd.pbi_status == 5,
    })
}
pub(super) fn parent_pid(pid: u32) -> Result<u32> {
    let bsd = pidinfo::<BSDInfo>(pid as i32, 0).map_err(|_| unavailable())?;
    if bsd.pbi_pid != pid {
        return Err(unavailable());
    }
    Ok(bsd.pbi_ppid)
}
pub(super) fn continuity(pid: u32) -> Result<super::continuity::Observation> {
    let bsd = pidinfo::<BSDInfo>(pid as i32, 0).map_err(|_| unavailable())?;
    if bsd.pbi_pid != pid
        || !matches!(bsd.pbi_status, 2..=5)
        || bsd.pbi_start_tvsec == 0
        || bsd.pbi_start_tvusec >= 1_000_000
        || bsd.pbi_uid != bsd.pbi_ruid
        || bsd.pbi_uid != bsd.pbi_svuid
    {
        return Err(unavailable());
    }
    Ok(super::continuity::Observation {
        birth: Birth {
            start: Start::Macos {
                seconds: bsd.pbi_start_tvsec,
                microseconds: bsd.pbi_start_tvusec,
            },
            exited: bsd.pbi_status == 5,
        },
        uid: bsd.pbi_uid,
    })
}
fn info(pid: u32) -> Result<(Start, u32)> {
    let bsd = pidinfo::<BSDInfo>(pid as i32, 0).map_err(|_| unavailable())?;
    // Darwin's SZOMB is 5. A zombie has exited even while its parent has not
    // reaped it; keeping a helper or process table entry cannot renew presence.
    if bsd.pbi_pid != pid
        || !matches!(bsd.pbi_status, 2..=4)
        || bsd.pbi_uid != bsd.pbi_ruid
        || bsd.pbi_uid != bsd.pbi_svuid
        || bsd.pbi_start_tvusec >= 1_000_000
    {
        return Err(unavailable());
    }
    Ok((
        Start::Macos {
            seconds: bsd.pbi_start_tvsec,
            microseconds: bsd.pbi_start_tvusec,
        },
        bsd.pbi_uid,
    ))
}
fn image(pid: u32) -> Result<Image> {
    let path = pidpath(pid as i32).map_err(|_| unavailable())?;
    let path = Path::new(&path);
    Image::new(path, path.metadata().map_err(|_| unavailable())?)
}

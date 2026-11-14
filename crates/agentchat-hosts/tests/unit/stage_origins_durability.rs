//! Inject held-parent sync refusal; visibility alone cannot retire history proof.
use super::{Directory, support::*};
use crate::staging::{FilePlan, OriginReaper, OriginRegistry, StagePurpose};
use std::{
    cell::RefCell,
    fs,
    io::Write,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
};

thread_local! {
    static REFUSED: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}
pub(crate) fn before_sync(path: &Path) -> agentchat_core::Result<()> {
    if REFUSED.with(|value| value.borrow().as_deref() == Some(path)) {
        return Err(crate::staging::unavailable());
    }
    Ok(())
}
pub(crate) struct Refusal;
impl Refusal {
    pub(crate) fn at(path: &Path) -> Self {
        REFUSED.with(|value| assert!(value.replace(Some(path.into())).is_none()));
        Self
    }
}
impl Drop for Refusal {
    fn drop(&mut self) {
        REFUSED.with(|value| *value.borrow_mut() = None);
    }
}
fn origins(home: &Path) -> usize {
    fs::read_dir(home.join("staging")).unwrap().count()
}

#[test]
fn local_finish_and_error_drop_keep_context_proof_until_parent_sync_succeeds() {
    for use_drop in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().canonicalize().unwrap().join("state");
        let parent = home.join("contexts");
        fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
        fs::DirBuilder::new().mode(0o700).create(&parent).unwrap();
        let held = Directory::owned(&parent).unwrap();
        let mut origin = OriginRegistry::new(&home)
            .begin_file(FilePlan {
                parent: &parent,
                stage: "original-stage".as_ref(),
                target: "canonical".as_ref(),
                purpose: StagePurpose::Context,
                alternate_parent: None,
                guard: None,
            })
            .unwrap();
        let mut file = held.create_record("original-stage".as_ref()).unwrap();
        file.write_all(DATA).unwrap();
        origin.seal(&file, DATA).unwrap();
        held.publish_new_io("original-stage".as_ref(), "canonical".as_ref())
            .unwrap();
        let refusal = Refusal::at(&parent);
        if use_drop {
            drop(origin);
            assert_eq!(origins(&home), 1, "sync refusal must retain the witness");
            drop(refusal);
        } else {
            assert!(origin.finish().is_err());
            assert_eq!(origins(&home), 1);
            drop(refusal);
            origin.finish().unwrap();
            assert_eq!(origins(&home), 0);
        }
        assert_eq!(fs::read(parent.join("canonical")).unwrap(), DATA);
        assert!(!parent.join("original-stage").exists());
    }
}

fn validate(bytes: &[u8], target: &std::ffi::OsStr) -> agentchat_core::Result<()> {
    if bytes == DATA && target == "canonical" {
        Ok(())
    } else {
        Err(crate::staging::unavailable())
    }
}

#[test]
fn killed_published_writer_keeps_its_origin_when_the_context_parent_cannot_sync() {
    let mut fixture = Fixture::start("context-published", StagePurpose::Context);
    fixture.kill();
    assert!(!fixture.stage().exists());
    assert_eq!(origins(&fixture.home), 1);
    let refusal = Refusal::at(&fixture.parent);
    let mut reaper = OriginReaper::new(&fixture.home).with_context_validator(validate);
    let page = reaper.step().unwrap();
    assert_eq!(
        page.removed, 0,
        "unsynced canonical visibility is insufficient"
    );
    assert!(page.preserved > 0);
    assert_eq!(origins(&fixture.home), 1);
    assert_eq!(fs::read(fixture.parent.join("canonical")).unwrap(), DATA);
    drop(refusal);
    let mut removed = 0;
    wait(|| {
        let page = reaper.step().unwrap();
        assert!(page.visited <= 8 && page.removed <= 16);
        removed += page.removed;
        origins(&fixture.home) == 0
    });
    assert_eq!(removed, 1);
    assert_eq!(fs::read(fixture.parent.join("canonical")).unwrap(), DATA);
}

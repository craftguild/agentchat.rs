//! Deterministic error/race controls; these are not real-host presence evidence.
use super::*;
use crate::claude::process::{Image, Start};

fn identity() -> ProcessIdentity {
    ProcessIdentity {
        pid: 1234,
        boot: Uuid::new_v4(),
        uid: 501,
        #[cfg(target_os = "linux")]
        start: Start::Linux { ticks: 123 },
        #[cfg(target_os = "macos")]
        start: Start::Macos {
            seconds: 123,
            microseconds: 456,
        },
        image: Image {
            path: b"/unavailable-replaced-image".to_vec(),
            device: 1,
            inode: 2,
        },
    }
}
fn live(id: &ProcessIdentity) -> Observation {
    Observation {
        birth: Birth {
            start: id.start.clone(),
            exited: false,
        },
        uid: id.uid,
    }
}
fn fail<T>() -> Result<T> {
    Err(super::super::unavailable())
}
fn run(
    id: &ProcessIdentity,
    boots: Vec<Result<Uuid>>,
    samples: Vec<Result<Observation>>,
    query: Option<rustix::io::Result<()>>,
) -> ContinuityState {
    let mut boots = boots.into_iter();
    let mut samples = samples.into_iter();
    let mut query = query;
    inspect(
        id,
        501,
        || boots.next().expect("unexpected boot probe"),
        |pid| {
            assert_eq!(pid, id.pid);
            samples.next().expect("unexpected credential/birth probe")
        },
        |pid| {
            assert_eq!(pid, id.pid);
            query.take().expect("unexpected permission probe")
        },
    )
}
#[test]
fn current_continuity_requires_stable_birth_and_user_but_never_image_fields() {
    let original = identity();
    for image in [
        original.image.clone(),
        Image {
            path: vec![],
            device: 0,
            inode: 0,
        },
        Image {
            path: vec![0],
            device: u64::MAX,
            inode: u64::MAX,
        },
    ] {
        let mut id = original.clone();
        id.image = image;
        let observation = live(&id);
        assert_eq!(
            run(
                &id,
                vec![Ok(id.boot), Ok(id.boot)],
                vec![Ok(observation.clone()), Ok(observation)],
                None
            ),
            ContinuityState::Current
        );
    }
}
#[test]
fn unsupported_users_and_user_changes_are_uncertain_without_birth_only_fallback() {
    let mut id = identity();
    id.uid = 502;
    assert_eq!(run(&id, vec![], vec![], None), ContinuityState::Unknown);
    let id = identity();
    let mut other = live(&id);
    other.uid = 502;
    assert_eq!(
        run(
            &id,
            vec![Ok(id.boot), Ok(id.boot)],
            vec![Ok(other.clone()), Ok(other)],
            None
        ),
        ContinuityState::Unknown
    );
}
#[test]
fn confirmed_exit_start_replacement_boot_replacement_and_typed_absence_are_gone() {
    let id = identity();
    let mut exited = live(&id);
    exited.birth.exited = true;
    let mut replaced = live(&id);
    match &mut replaced.birth.start {
        Start::Linux { ticks } => *ticks += 1,
        Start::Macos { seconds, .. } => *seconds += 1,
    }
    for sample in [exited, replaced] {
        assert_eq!(
            run(
                &id,
                vec![Ok(id.boot), Ok(id.boot)],
                vec![Ok(sample.clone()), Ok(sample)],
                None
            ),
            ContinuityState::Gone
        );
    }
    let boot = Uuid::new_v4();
    assert_eq!(
        run(&id, vec![Ok(boot), Ok(boot)], vec![], None),
        ContinuityState::Gone
    );
    assert_eq!(
        run(
            &id,
            vec![Ok(id.boot), Ok(id.boot)],
            vec![fail()],
            Some(Err(Errno::SRCH))
        ),
        ContinuityState::Gone
    );
}
#[test]
fn unreadable_malformed_and_racing_observations_never_guess_authority_or_death() {
    let id = identity();
    let sample = live(&id);
    for query in [
        Ok(()),
        Err(Errno::PERM),
        Err(Errno::IO),
        Err(Errno::INVAL),
        Err(Errno::INTR),
    ] {
        assert_eq!(
            run(
                &id,
                vec![Ok(id.boot), Ok(id.boot)],
                vec![fail()],
                Some(query)
            ),
            ContinuityState::Unknown
        );
    }
    let mut changed = live(&id);
    changed.uid += 1;
    for second in [fail(), Ok(changed)] {
        assert_eq!(
            run(
                &id,
                vec![Ok(id.boot), Ok(id.boot)],
                vec![Ok(sample.clone()), second],
                None
            ),
            ContinuityState::Unknown
        );
    }
    assert_eq!(
        run(&id, vec![fail()], vec![], None),
        ContinuityState::Unknown
    );
    assert_eq!(
        run(&id, vec![Ok(Uuid::nil())], vec![], None),
        ContinuityState::Unknown
    );
    for final_boot in [fail(), Ok(Uuid::new_v4())] {
        assert_eq!(
            run(
                &id,
                vec![Ok(id.boot), final_boot],
                vec![fail()],
                Some(Err(Errno::SRCH))
            ),
            ContinuityState::Unknown
        );
    }
    for pid in [0, u32::MAX] {
        let mut bad = id.clone();
        bad.pid = pid;
        assert_eq!(run(&bad, vec![], vec![], None), ContinuityState::Unknown);
    }
}

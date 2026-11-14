//! Synthetic failures and races; no mock result is real-host presence evidence.
use super::*;
use crate::claude::process::Image;

fn identity() -> ProcessIdentity {
    #[cfg(target_os = "linux")]
    let start = Start::Linux { ticks: 123 };
    #[cfg(target_os = "macos")]
    let start = Start::Macos {
        seconds: 123,
        microseconds: 456,
    };
    ProcessIdentity {
        pid: 1234,
        boot: Uuid::new_v4(),
        start,
        uid: 501,
        image: Image {
            path: b"/synthetic-native".to_vec(),
            device: 1,
            inode: 2,
        },
    }
}
fn run(
    id: &ProcessIdentity,
    boots: Vec<Result<Uuid>>,
    samples: Vec<Result<Birth>>,
    query: Option<rustix::io::Result<()>>,
) -> BirthState {
    let mut boots = boots.into_iter();
    let mut samples = samples.into_iter();
    let mut query = query;
    inspect(
        id,
        || boots.next().expect("unexpected boot probe"),
        |pid| {
            assert_eq!(pid, id.pid);
            samples.next().expect("unexpected birth probe")
        },
        |pid| {
            assert_eq!(pid, id.pid);
            query
                .take()
                .expect("unexpected or repeated permission query")
        },
    )
}
fn failure<T>() -> Result<T> {
    Err(crate::claude::process::unavailable())
}
fn live(id: &ProcessIdentity) -> Birth {
    Birth {
        start: id.start.clone(),
        exited: false,
    }
}
fn changed(id: &ProcessIdentity) -> Birth {
    let mut birth = live(id);
    match &mut birth.start {
        Start::Linux { ticks } => *ticks += 1,
        Start::Macos { seconds, .. } => *seconds += 1,
    }
    birth
}
#[test]
fn unreadable_denied_and_other_probe_errors_are_not_death() {
    let id = identity();
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
                vec![failure()],
                Some(query)
            ),
            BirthState::Unknown
        );
    }
    assert_eq!(run(&id, vec![failure()], vec![], None), BirthState::Unknown);
    assert_eq!(
        run(&id, vec![Ok(Uuid::nil())], vec![], None),
        BirthState::Unknown
    );
}
#[test]
fn stable_boot_start_exit_and_typed_absence_are_distinct() {
    let id = identity();
    let current = live(&id);
    assert_eq!(
        run(
            &id,
            vec![Ok(id.boot), Ok(id.boot)],
            vec![Ok(current.clone()), Ok(current)],
            None
        ),
        BirthState::Present
    );
    for observed in [
        changed(&id),
        Birth {
            start: id.start.clone(),
            exited: true,
        },
    ] {
        assert_eq!(
            run(
                &id,
                vec![Ok(id.boot), Ok(id.boot)],
                vec![Ok(observed.clone()), Ok(observed)],
                None
            ),
            BirthState::Gone
        );
    }
    assert_eq!(
        run(
            &id,
            vec![Ok(id.boot), Ok(id.boot)],
            vec![failure()],
            Some(Err(Errno::SRCH))
        ),
        BirthState::Gone
    );
    let other_boot = Uuid::new_v4();
    assert_eq!(
        run(&id, vec![Ok(other_boot), Ok(other_boot)], vec![], None),
        BirthState::Gone
    );
}
#[test]
fn racing_boot_or_partial_birth_observations_preserve_metadata() {
    let id = identity();
    let current = live(&id);
    for final_boot in [failure(), Ok(Uuid::new_v4())] {
        assert_eq!(
            run(
                &id,
                vec![Ok(id.boot), final_boot],
                vec![failure()],
                Some(Err(Errno::SRCH))
            ),
            BirthState::Unknown
        );
    }
    for second in [
        failure(),
        Ok(changed(&id)),
        Ok(Birth {
            start: id.start.clone(),
            exited: true,
        }),
    ] {
        assert_eq!(
            run(
                &id,
                vec![Ok(id.boot), Ok(id.boot)],
                vec![Ok(current.clone()), second],
                None
            ),
            BirthState::Unknown
        );
    }
    let other_boot = Uuid::new_v4();
    assert_eq!(
        run(&id, vec![Ok(other_boot), failure()], vec![], None),
        BirthState::Unknown
    );
}
#[test]
fn malformed_birth_tokens_never_issue_a_kernel_query() {
    let base = identity();
    for pid in [0, u32::MAX] {
        let mut id = base.clone();
        id.pid = pid;
        assert_eq!(run(&id, vec![], vec![], None), BirthState::Unknown);
    }
    let mut id = base;
    id.boot = Uuid::nil();
    assert_eq!(run(&id, vec![], vec![], None), BirthState::Unknown);
}

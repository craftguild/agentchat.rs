//! Adversarial Linux kernel metadata parser cases.
use super::{birth_stat, stat, uids};

fn record(name: &str, state: &str, start: &str) -> String {
    format!(
        "123 ({name}) {state} {} {start} 0 0",
        vec!["0"; 18].join(" ")
    )
}

#[test]
fn proc_stat_uses_the_last_comm_delimiter_and_exact_start_field() {
    for name in ["plain", "some ) ( tricky name)", "line\nbreak )"] {
        assert_eq!(
            stat(123, &record(name, "S", "9223372036854775807")).unwrap(),
            9_223_372_036_854_775_807
        );
    }
    assert!(stat(124, &record("plain", "S", "9")).is_err());
}

#[test]
fn dead_unknown_truncated_and_overflowed_proc_records_refuse() {
    for state in ["Z", "X", "x", "?", "SS", ""] {
        assert!(stat(123, &record("x", state, "9")).is_err());
    }
    for start in ["-1", "18446744073709551616", "bad"] {
        assert!(stat(123, &record("x", "S", start)).is_err());
    }
    for value in ["", "123", "123 (x)", "123 (x) S 0", "abc (x) S"] {
        assert!(stat(123, value).is_err());
    }
}

#[test]
fn proc_uid_requires_one_complete_unchanged_user_tuple() {
    assert_eq!(
        uids("Name:\tignored\nUid:\t501 501 501 501\nGid:\t20").unwrap(),
        501
    );
    for value in [
        "",
        "Uid: 501 501 501",
        "Uid: 501 501 501 501 501",
        "Uid: 501 0 501 501",
        "Uid: x x x x",
        "Uid: 501 501 501 501\nUid: 501 501 501 501",
    ] {
        assert!(uids(value).is_err());
    }
}

#[test]
fn birth_parser_distinguishes_kernel_exit_from_live_process_states() {
    for state in ["Z", "X", "x"] {
        assert_eq!(
            birth_stat(123, &record("dead ) name", state, "91")).unwrap(),
            (91, true)
        );
        assert!(stat(123, &record("dead ) name", state, "91")).is_err());
    }
    for state in ["R", "S", "D", "T", "t", "K", "W", "P", "I"] {
        assert_eq!(
            birth_stat(123, &record("live\n)", state, "91")).unwrap(),
            (91, false)
        );
    }
}
#[test]
fn birth_parser_never_turns_corrupt_kernel_metadata_into_exit() {
    for state in ["?", "ZZ", "", "S Z"] {
        assert!(birth_stat(123, &record("x", state, "91")).is_err());
    }
    for start in ["-1", "18446744073709551616", "bad"] {
        assert!(birth_stat(123, &record("x", "Z", start)).is_err());
    }
    assert!(birth_stat(124, &record("x", "Z", "91")).is_err());
    assert!(birth_stat(123, "123 (x) Z 0").is_err());
}

//! Shared validation boundaries from T04, T06, T12 and T17.

use agentchat_core::validation;

#[test]
fn slugs_and_reserved_handles() {
    assert_eq!(validation::handle("Blue-Otter9").unwrap(), "blue-otter9");
    for s in [
        "",
        "all",
        "AGENTCHAT",
        "System",
        "here",
        "channel",
        "9cat",
        "-cat",
        "cat-",
        "two--cats",
        "é",
        "a_b",
    ] {
        assert!(validation::handle(s).is_err(), "{s}");
    }
    assert!(validation::handle(&"a".repeat(32)).is_ok());
    assert!(validation::handle(&"a".repeat(33)).is_err());
    assert!(validation::channel(&"a".repeat(64)).is_ok());
    assert!(validation::channel(&"a".repeat(65)).is_err());
    assert!(validation::channel("system").is_ok());
}

#[test]
fn byte_and_numeric_limits() {
    assert!(validation::body(" \n\t").is_err());
    assert!(validation::body(&"🦀".repeat(16_384)).is_ok());
    assert!(validation::body(&"🦀".repeat(16_385)).is_err());
    assert_eq!(validation::ttl(None).unwrap(), 600);
    for ttl in [0, 29, 3_601, u32::MAX] {
        assert!(validation::ttl(Some(ttl)).is_err());
    }
    for ttl in [30, 600, 3_600] {
        assert_eq!(validation::ttl(Some(ttl)).unwrap(), ttl);
    }
    assert_eq!(validation::page_limit(None).unwrap(), 50);
    for n in [0, 201, usize::MAX] {
        assert!(validation::page_limit(Some(n)).is_err());
    }
    for n in [0, 201, usize::MAX] {
        assert!(validation::bulk_len(n).is_err());
    }
}

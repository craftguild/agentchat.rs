//! Public cursors must not lose precision in JavaScript clients.

use agentchat_core::sequence::{Cursor, Sequence};

#[test]
fn decimal_strings_preserve_full_i64_range() {
    let seq = Sequence::new(i64::MAX).unwrap();
    let encoded = serde_json::to_string(&seq).unwrap();
    assert_eq!(encoded, "\"9223372036854775807\"");
    assert_eq!(serde_json::from_str::<Sequence>(&encoded).unwrap(), seq);
    assert_eq!(Cursor::START.get(), 0);
    assert_eq!(seq.cursor().get(), i64::MAX);
    for invalid in ["-1", "", "+1", "1.0", "9223372036854775808", " 1"] {
        assert!(Cursor::parse(invalid).is_err());
    }
    for invalid in ["0", "1", "-1", "\"0\"", "null"] {
        assert!(serde_json::from_str::<Sequence>(invalid).is_err());
    }
    assert_eq!(
        serde_json::from_str::<Cursor>("\"0\"").unwrap(),
        Cursor::START
    );
}

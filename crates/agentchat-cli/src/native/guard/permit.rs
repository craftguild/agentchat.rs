//! Native module permits are short-lived and bound to one exact context/notice.
use agentchat_core::{Error, ErrorCode};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Permit {
    pub root: Uuid,
    pub generation: u64,
    pub native_generation: u64,
    pub token: String,
    pub issued: u64,
    pub context_id: Uuid,
    pub incarnation: Uuid,
    pub dispatch: Uuid,
    pub expected_turn_id: Option<String>,
    #[serde(default)]
    pub epoch: Option<u64>,
    #[serde(default)]
    pub phase: Option<String>,
}
impl Permit {
    pub fn valid(&self, now: u64, root: Uuid) -> bool {
        self.root == root
            && !self.context_id.is_nil()
            && !self.incarnation.is_nil()
            && !self.dispatch.is_nil()
            && self.generation > 0
            && self.generation <= 9_007_199_254_740_991
            && self.native_generation > 0
            && token(&self.token)
            && now >= self.issued
            && now - self.issued < 2000
            && self
                .expected_turn_id
                .as_ref()
                .is_none_or(|id| !id.is_empty() && id.len() <= 128)
    }
    pub fn matches_notice(&self, text: &str) -> bool {
        let prefix = "AGENTCHAT_TOKEN_NOTICE root=";
        if text.matches(prefix).count() != 1 {
            return false;
        }
        let tail = text.split_once(prefix).map(|(_, tail)| tail).unwrap_or("");
        let mut words = tail.split_whitespace();
        let root = words.next().and_then(|s| Uuid::parse_str(s).ok());
        let generation = words
            .next()
            .and_then(|s| s.strip_prefix("generation="))
            .and_then(|s| s.parse::<u64>().ok());
        let value = words
            .next()
            .and_then(|s| s.strip_prefix("token="))
            .unwrap_or("");
        let value = value
            .chars()
            .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
            .collect::<String>();
        root == Some(self.root) && generation == Some(self.generation) && value == self.token
    }
}
fn token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
pub(super) fn required() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "The native notice no longer has an exact live guard permit.",
    )
}

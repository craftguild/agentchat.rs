//! Optional observations cannot determine application/native routing outcomes.
use serde_json::Value;
use std::sync::Arc;
/// Optional bounded, nonblocking evidence consumer. It is never authority.
/// Production may omit detailed observations; sinks must not include bodies,
/// execute application work or panic. Failures must be handled inside the sink.
pub trait Trace: Send + Sync {
    /// Observe native routing/lifecycle metadata after its independent checks.
    fn record(&self, event: Value);
}
#[derive(Clone)]
pub(crate) struct Diagnostics(Option<Arc<dyn Trace>>);
impl Diagnostics {
    pub(crate) fn new(sink: Option<Arc<dyn Trace>>) -> Self {
        Self(sink)
    }
    pub(crate) fn record(&self, event: Value) {
        if let Some(sink) = &self.0 {
            sink.record(event);
        }
    }
}

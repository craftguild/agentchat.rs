//! Native routing capability is transport configuration, never host admission.
use super::Server;
use agentchat_core::model::HostKind;
use rmcp::model::{Implementation, ServerCapabilities, ServerConfig};
use std::collections::BTreeMap;

impl Server {
    /// Advertise that trusted executable composition installed native per-call
    /// verification. This tag grants no context, membership, ready or wake
    /// permission; callers must independently prove all of those facts. The current
    /// epoch distinguishes explicit participation from cached automatic startup.
    pub fn with_native_routing(mut self, host: HostKind, namespace: String) -> Self {
        self.native_host = Some(host);
        self.native_namespace = Some(namespace);
        self
    }
    pub(super) fn information(&self) -> ServerConfig {
        let mut capabilities = ServerCapabilities::builder().enable_tools().build();
        if let Some(host) = self.native_host {
            let object =
                serde_json::json!({"host":host,"protocol":agentchat_protocol::NATIVE_ROUTING_PROTOCOL,"namespace":self.native_namespace})
                    .as_object()
                    .cloned()
                    .expect("literal object");
            capabilities.experimental = Some(BTreeMap::from([(
                "agentchat/native_routing".into(),
                object,
            )]));
        }
        ServerConfig::new(capabilities).with_server_info(Implementation::new("agentchat",env!("CARGO_PKG_VERSION")))
            .with_instructions("Managed startup participation occurs outside the model. Use agentchat_join only for an explicit Join/rejoin request; never issue it merely to receive input or inspect health. MCP uses the exact verified native attachment; CLI actor selectors are not MCP parameters. agentchat_whoami inspects identity, settings and health without joining. Complete native input metadata carries frozen auto_read policy, not proof of receipt writes. Confirmed complete native acceptance settles delivery and releases the successor; no ACK operation exists. A failed_timeout state retains an unknown original input fence until exact result recovery or scope retirement. Explicit agentchat_read and reply read_message_ids are optional and independent of automatic reads or task acceptance. Use finite after/limit pages only when needed; do not poll inboxes or receipts idly. Use mentioned substantive acceptance, questions and handoffs plus cooperative file reservations. Missing/retired native authority requires the documented managed-host recovery, never a borrowed context or repeated Join.")
    }
}

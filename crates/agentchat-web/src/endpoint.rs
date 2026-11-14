//! Typed schema assembly and explicit finite SDK resource limits.

use crate::{
    render::Renderer,
    resolvers::{Fields, Types},
};
use agentchat_core::{Error, ErrorCode};
use agentchat_protocol::{viewer::ViewerReader, viewer_write::ViewerWriter};
use agentchat_viewer_contract::Resolvers;
use chigiri::{Endpoint, EndpointBuildError, ExecutionLimits, FieldError, SubscriptionLimits};
use std::{sync::Arc, time::Duration};

/// Build the product schema with context-free reads and no write capability.
/// This creates no listener or agent identity. Use Chigiri's managed Axum service
/// and public shutdown hook when composing its native server.
pub fn endpoint(reader: Arc<dyn ViewerReader>) -> Result<Endpoint<bool>, EndpointBuildError> {
    endpoint_with_writer(reader, None)
}

/// Build the same schema with an explicit fixed-project human action capability.
pub fn endpoint_with_writer(
    reader: Arc<dyn ViewerReader>,
    writer: Option<Arc<dyn ViewerWriter>>,
) -> Result<Endpoint<bool>, EndpointBuildError> {
    let fields = Fields {
        writer,
        reader,
        renderer: Renderer::default(),
    };
    let resolvers = Resolvers::<bool, Types>::builder(map_error)
        .with_mutation(fields.clone())
        .with_created_channel(fields.clone())
        .with_posted_message(fields.clone())
        .with_post_warning(fields.clone())
        .with_query(fields.clone())
        .with_subscription(fields.clone())
        .with_checkpoint(fields.clone())
        .with_project(fields.clone())
        .with_channel(fields.clone())
        .with_channel_view(fields.clone())
        .with_channel_page(fields.clone())
        .with_author(fields.clone())
        .with_agent(fields.clone())
        .with_actor(fields.clone())
        .with_actor_page(fields.clone())
        .with_agent_page(fields.clone())
        .with_message_record(fields.clone())
        .with_delivery_receipt(fields.clone())
        .with_receipt_read(fields.clone())
        .with_recipient_receipt(fields.clone())
        .with_receipt_page(fields.clone())
        .with_message_detail(fields.clone())
        .with_message(fields.clone())
        .with_message_window(fields.clone())
        .with_pinned_root(fields.clone())
        .with_change(fields.clone())
        .with_change_batch(fields)
        .build();
    Endpoint::builder(resolvers)
        .context_factory(access)
        .body_limit(512 * 1024)
        .response_byte_limit(1024 * 1024)
        .execution_timeout(Duration::from_secs(10))
        .execution_limits(ExecutionLimits {
            max_document_bytes: 16 * 1024,
            max_parser_nesting: 32,
            max_operation_depth: 12,
            max_expanded_fields: 128,
            max_aliases: 4,
            max_variables: 16,
            max_directives: 16,
            max_diagnostics: 16,
            max_concurrent_fields: 8,
            max_list_items_per_field: 200,
            max_custom_scalar_output_depth: 16,
            max_output_nodes: 8192,
        })
        .subscription_limits(SubscriptionLimits {
            max_active_subscriptions: 4,
            max_pending_operation_setups: 4,
            max_queued_events: 2,
            max_queued_bytes: 2 * 1024 * 1024,
            max_encoded_event_bytes: 1024 * 1024,
            max_endpoint_encoded_bytes: 16 * 1024 * 1024,
            max_queue_age: Duration::from_secs(10),
            send_no_progress_timeout: Duration::from_secs(5),
            setup_timeout: Duration::from_secs(10),
            event_timeout: Duration::from_secs(10),
            max_retained_metadata_bytes: 8 * 1024,
            heartbeat_interval: Duration::from_secs(15),
        })
        .build()
}

fn map_error(error: Error) -> FieldError {
    let code = match error.code {
        ErrorCode::InvalidArgument => "invalid_argument",
        ErrorCode::InvalidName => "invalid_name",
        ErrorCode::InvalidThread => "invalid_thread",
        ErrorCode::NotFound => "not_found",
        ErrorCode::ProjectBindingMismatch => "project_binding_mismatch",
        ErrorCode::Busy => "busy",
        ErrorCode::RuntimeUnavailable => "runtime_unavailable",
        ErrorCode::IoError => "io_error",
        ErrorCode::StorageError => "storage_error",
        ErrorCode::IntegrityError => "integrity_error",
        _ => "viewer_request_failed",
    };
    // Open domain error details are permitted at this final presentation edge.
    // Expose only the reset flag, never storage paths or host process details.
    let reset = error
        .details
        .get("reset_required")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    FieldError::public("Viewer request could not be completed.")
        .code(code)
        .extension("resetRequired", reset)
}

fn access(
    request: chigiri::HttpRequestContext<'_>,
) -> chigiri::BoxFuture<
    '_,
    std::result::Result<chigiri::PreparedContext<bool, ()>, chigiri::PrepareError>,
> {
    let headers = request.transport().headers();
    let one = |name: &str| {
        let mut values = headers.iter().filter(|h| h.name.eq_ignore_ascii_case(name));
        let value = values.next().map(|h| h.value);
        if values.next().is_some() { None } else { value }
    };
    let allowed = match (one("origin"), one("host"), one("content-type")) {
        (Some(origin), Some(host), Some(content)) => {
            let expected = [b"http://".as_slice(), host].concat();
            origin == expected
                && content.split(|b| *b == b';').next() == Some(b"application/json".as_slice())
        }
        _ => false,
    };
    Box::pin(async move { Ok(chigiri::PreparedContext::new(allowed, ())) })
}

//! Local HTTP policy runs before any GraphQL setup or runtime read.

use crate::assets;
use axum::{
    Router,
    body::Body,
    extract::State,
    http::{Method, Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post_service},
};
use chigiri::{Endpoint, ManagedServiceError};
use futures_util::StreamExt;
use std::sync::Arc;
use tokio::sync::Semaphore;

#[derive(Clone)]
struct Gate {
    authority: String,
    origin: String,
    requests: Arc<Semaphore>,
}

pub(crate) fn router(
    endpoint: Endpoint<bool>,
    authority: String,
    context: crate::ViewerContext,
) -> Result<Router, ManagedServiceError> {
    let context = serde_json::to_vec(&context).expect("display metadata strings serialize");
    let gate = Gate {
        origin: format!("http://{authority}"),
        authority,
        requests: Arc::new(Semaphore::new(8)),
    };
    Ok(Router::new()
        .route(
            "/graphql",
            post_service(chigiri_axum::service(endpoint)?).get(asset),
        )
        .route(
            "/viewer-context.json",
            get(move || async move {
                (
                    [
                        (header::CACHE_CONTROL, "no-store"),
                        (header::CONTENT_TYPE, "application/json"),
                    ],
                    context,
                )
            }),
        )
        .fallback(asset)
        .layer(middleware::from_fn_with_state(gate, guard)))
}

async fn guard(State(gate): State<Gate>, request: Request<Body>, next: Next) -> Response {
    if !exact(&request, header::HOST, &gate.authority, true)
        || !exact(&request, header::ORIGIN, &gate.origin, false)
        || request
            .headers()
            .get("sec-fetch-site")
            .is_some_and(|site| site == "cross-site")
    {
        return secured(StatusCode::FORBIDDEN.into_response());
    }
    if request.uri().query().is_some() {
        return secured(StatusCode::NOT_FOUND.into_response());
    }
    // GET/HEAD /graphql is also the document for a channel named graphql;
    // GraphQL execution remains POST-only, with no query-string admission.
    let graphql = request.uri().path() == "/graphql"
        && request.method() != Method::GET
        && request.method() != Method::HEAD;
    if graphql && request.method() != Method::POST {
        return secured(StatusCode::METHOD_NOT_ALLOWED.into_response());
    }
    if !graphql && request.method() != Method::GET && request.method() != Method::HEAD {
        return secured(StatusCode::METHOD_NOT_ALLOWED.into_response());
    }
    let permit = if graphql {
        match gate.requests.clone().try_acquire_owned() {
            Ok(permit) => Some(permit),
            Err(_) => return secured(StatusCode::SERVICE_UNAVAILABLE.into_response()),
        }
    } else {
        None
    };
    let response = secured(next.run(request).await);
    let Some(permit) = permit else {
        return response;
    };
    let (parts, body) = response.into_parts();
    // Admission remains owned through the complete HTTP/SSE body, not just its
    // response headers. Dropping it propagates to the SDK's owned source.
    let stream = futures_util::stream::unfold(
        (body.into_data_stream(), permit),
        |(mut body, permit)| async move {
            let chunk = body.next().await?;
            Some((chunk, (body, permit)))
        },
    );
    Response::from_parts(parts, Body::from_stream(stream))
}
fn exact(
    request: &Request<Body>,
    name: header::HeaderName,
    expected: &str,
    required: bool,
) -> bool {
    let mut values = request.headers().get_all(name).iter();
    match values.next() {
        None => !required,
        Some(value) => value.as_bytes() == expected.as_bytes() && values.next().is_none(),
    }
}
async fn asset(request: Request<Body>) -> Response {
    // Only document routes use the SPA shell. Missing assets and API paths
    // retain their 404s; a location never chooses a project or performs reads.
    let path = request.uri().path();
    let document = viewer_path(path);
    let Some(asset) = assets::get(if document { "/" } else { path }) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let etag = format!("\"{}\"", asset.sha256);
    if request
        .headers()
        .get(header::IF_NONE_MATCH)
        .is_some_and(|value| value.as_bytes() == etag.as_bytes())
    {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    let mut response = if request.method() == Method::HEAD {
        Body::empty()
    } else {
        Body::from(asset.bytes)
    }
    .into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static(asset.mime),
    );
    headers.insert(
        header::ETAG,
        header::HeaderValue::from_str(&etag).expect("generated hexadecimal digest"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static(if asset.path.starts_with("/assets/") {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        }),
    );
    response
}
fn viewer_path(path: &str) -> bool {
    let Some(tail) = path.strip_prefix('/') else {
        return false;
    };
    let mut parts = tail.split('/');
    let Some(channel) = parts.next() else {
        return false;
    };
    if agentchat_core::validation::channel(channel).is_err() {
        return false;
    }
    match parts.next() {
        None => true,
        Some(root) => {
            root.len() == 36 && uuid::Uuid::parse_str(root).is_ok() && parts.next().is_none()
        }
    }
}
fn secured(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_SECURITY_POLICY, header::HeaderValue::from_static(
        "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self'; font-src 'self'; base-uri 'none'; object-src 'none'; frame-ancestors 'none'; form-action 'none'"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        header::HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        "cross-origin-resource-policy",
        header::HeaderValue::from_static("same-origin"),
    );
    response
}

//! Product HTTP policy and native cleanup, with the actual embedded assets/service.
mod support;

use agentchat_web::Viewer;
use serde_json::json;
use std::{
    net::SocketAddr,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use support::{
    reader::{Fixture, Reader},
    wire::Http,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::oneshot,
};

async fn raw(address: SocketAddr, request: String) -> Vec<u8> {
    let mut socket = TcpStream::connect(address).await.unwrap();
    socket.write_all(request.as_bytes()).await.unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), socket.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    assert!(bytes.len() <= 1024 * 1024);
    bytes
}
fn address(url: &str) -> SocketAddr {
    url.trim_start_matches("http://")
        .trim_end_matches('/')
        .parse()
        .unwrap()
}

#[tokio::test]
async fn embedded_assets_and_host_origin_path_guards_run_before_reader_setup() {
    let fixture = Fixture::new();
    let viewer = Viewer::bind_with_context(
        Arc::new(Reader(fixture.clone())),
        agentchat_web::ViewerContext {
            directory_name: Some("<chat> & 日本語".into()),
        },
    )
    .await
    .unwrap();
    let address = address(viewer.url());
    assert!(address.ip().is_loopback());
    let (stop, receive) = oneshot::channel();
    let task = tokio::spawn(viewer.serve(async {
        let _ = receive.await;
    }));
    let metadata = raw(
        address,
        format!(
            "GET /viewer-context.json HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
        ),
    )
    .await;
    let metadata = String::from_utf8(metadata).unwrap();
    assert!(metadata.starts_with("HTTP/1.1 200"));
    assert!(metadata.to_lowercase().contains("cache-control: no-store"));
    assert!(metadata.contains("<chat> & 日本語"));
    let index = raw(
        address,
        format!("GET / HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"),
    )
    .await;
    let index = String::from_utf8(index).unwrap();
    assert!(index.starts_with("HTTP/1.1 200"));
    assert!(index.contains("<div id=\"root\"></div>"));
    assert!(index.contains("script-src 'self'"));
    assert!(index.contains("frame-ancestors 'none'"));
    assert!(
        index
            .to_lowercase()
            .contains("x-content-type-options: nosniff")
    );
    for path in [
        "/dev",
        "/dev/00000000-0000-0000-0000-000000000083",
        "/archived-channel",
        "/graphql",
    ] {
        let response = raw(
            address,
            format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"),
        )
        .await;
        let response = String::from_utf8(response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{path}");
        assert!(response.contains("<div id=\"root\"></div>"));
    }
    for path in [
        "/dev/",
        "/dev/not-a-root",
        "/dev/00000000-0000-0000-0000-000000000083/extra",
        "/c/dev",
        "/assets/missing.js",
        "/unknown.js",
    ] {
        let response = raw(
            address,
            format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"),
        )
        .await;
        assert!(response.starts_with(b"HTTP/1.1 404"), "{path}");
    }
    let javascript = index
        .split("src=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    let asset = raw(
        address,
        format!("GET {javascript} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"),
    )
    .await;
    assert!(asset.starts_with(b"HTTP/1.1 200"));
    assert!(asset.len() > 20_000);
    let notices = raw(
        address,
        format!(
            "GET /third-party-notices.txt HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
        ),
    )
    .await;
    let notices = String::from_utf8(notices).unwrap();
    assert!(notices.starts_with("HTTP/1.1 200"));
    assert!(notices.to_lowercase().contains("text/plain; charset=utf-8"));
    assert!(notices.contains("react@19.3.1 (MIT)"));
    assert!(notices.contains("Copyright (c) Meta Platforms, Inc. and affiliates."));
    assert!(notices.contains("Permission is hereby granted, free of charge"));
    for (method, path, headers, status) in [
        (
            "GET",
            "/viewer-context.json",
            "Origin: http://attacker.invalid\r\n".into(),
            403,
        ),
        ("POST", "/viewer-context.json", String::new(), 405),
        (
            "GET",
            "/viewer-context.json?project=other",
            String::new(),
            404,
        ),
        ("PUT", "/graphql", String::new(), 405),
        (
            "POST",
            "/graphql?project=/private/secret",
            String::new(),
            404,
        ),
        ("GET", "/../config.toml", String::new(), 404),
        (
            "POST",
            "/graphql",
            "Origin: http://attacker.invalid\r\n".into(),
            403,
        ),
        ("POST", "/graphql", "Origin: null\r\n".into(), 403),
        (
            "POST",
            "/graphql",
            format!("Origin: http://{address}\r\nOrigin: http://{address}\r\n"),
            403,
        ),
        (
            "POST",
            "/graphql",
            "Sec-Fetch-Site: cross-site\r\n".into(),
            403,
        ),
    ] {
        let checkpoint = fixture.current();
        let body = json!({"query":"subscription($after:CheckpointInput!){changes(after:$after){hasMore}}",
            "variables":{"after":{"projectId":checkpoint.project_id,"seq":checkpoint.seq,"eventId":checkpoint.event_id}}}).to_string();
        let response = raw(address, format!("{method} {path} HTTP/1.1\r\nHost: {address}\r\n{headers}Content-Type: application/json\r\nAccept: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())).await;
        assert!(
            response.starts_with(format!("HTTP/1.1 {status}").as_bytes()),
            "{}",
            String::from_utf8_lossy(&response)
        );
    }
    let wrong_host = raw(
        address,
        "GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n".into(),
    )
    .await;
    assert!(wrong_host.starts_with(b"HTTP/1.1 403"));
    assert_eq!(fixture.starts.load(Ordering::SeqCst), 0);
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn stream_admission_precedes_setup_and_shutdown_closes_idle_native_connections() {
    let fixture = Fixture::new();
    let viewer = Viewer::bind(Arc::new(Reader(fixture.clone())))
        .await
        .unwrap();
    let address = address(viewer.url());
    let (stop, receive) = oneshot::channel();
    let task = tokio::spawn(viewer.serve(async {
        let _ = receive.await;
    }));
    let after = fixture.current();
    let query = "subscription($after:CheckpointInput!){ changes(after:$after){ hasMore } }";
    let variables =
        json!({"after":{"projectId":after.project_id,"seq":after.seq,"eventId":after.event_id}});
    let mut streams = Vec::new();
    for _ in 0..4 {
        let stream = Http::open(address, query, variables.clone(), true).await;
        assert_eq!(stream.response.status(), 200);
        streams.push(stream);
    }
    let denied = Http::open(address, query, variables, true).await;
    assert_ne!(denied.response.status(), 200);
    drop(denied);
    assert_eq!(fixture.starts.load(Ordering::SeqCst), 4);
    let mut idle = TcpStream::connect(address).await.unwrap();
    idle.write_all(b"GET / HTTP/1.1\r\nHost:").await.unwrap();
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(6), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let mut byte = [0];
    let read = tokio::time::timeout(Duration::from_secs(1), idle.read(&mut byte))
        .await
        .unwrap();
    assert!(matches!(read, Ok(0) | Err(_)));
    assert_eq!(fixture.drops.load(Ordering::SeqCst), 4);
    drop(streams);
}

//! Lazy-start races converge on one OS-owned, handshaken daemon incarnation.

mod support;
use agentchat_core::time::SystemClock;
use agentchat_runtime::daemon::{RuntimePaths, Server, connect_or_start};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use support::{
    Fixture, SyntheticHost,
    runtime::{Running, options},
};

#[tokio::test]
async fn an_existing_checked_owner_does_not_invoke_the_launcher() {
    let r = Running::new(options()).await;
    let calls = AtomicUsize::new(0);
    let client = connect_or_start(r.handle.paths(), r.project.identity().id, || async {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(client.instance_id(), r.handle.instance_id());
    drop(client);
    r.stop().await;
}

#[tokio::test]
async fn concurrent_lazy_launchers_return_the_same_runtime_incarnation() {
    let f = Fixture::new();
    let p = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap()
        .project;
    let paths = RuntimePaths::for_project(&p);
    let owners = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let mut clients = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let manager = f.manager.clone();
        let project = p.clone();
        let paths = paths.clone();
        let owners = owners.clone();
        clients.spawn(async move {
            connect_or_start(&paths, project.identity().id, || async move {
                let server = Server::bind(
                    manager,
                    project,
                    Arc::new(SyntheticHost),
                    Arc::new(SystemClock),
                    options(),
                )
                .await?;
                let handle = server.handle();
                owners
                    .lock()
                    .await
                    .push((handle, tokio::spawn(server.run())));
                Ok(())
            })
            .await
        });
    }
    let mut results = Vec::new();
    while let Some(client) = clients.join_next().await {
        results.push(client.unwrap().unwrap());
    }
    let mut owners = owners.lock().await;
    assert_eq!(owners.len(), 1);
    let (handle, task) = owners.pop().unwrap();
    assert!(
        results
            .iter()
            .all(|client| client.instance_id() == handle.instance_id())
    );
    drop(results);
    handle.shutdown();
    task.await.unwrap().unwrap();
    assert!(!paths.socket.exists());
}

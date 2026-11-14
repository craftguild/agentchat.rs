//! Real OS ownership, short paths and preservation of unrelated endpoint objects.

mod support;
use agentchat_core::{ErrorCode, time::SystemClock};
use agentchat_runtime::{
    daemon::{RuntimePaths, Server},
    project::Project,
};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    sync::Arc,
};
use support::{Fixture, SyntheticHost, runtime::options};
use tokio::net::UnixListener;

async fn project(f: &Fixture) -> Project {
    f.manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap()
        .project
}
async fn bind(f: &Fixture, p: &Project) -> agentchat_core::Result<Server> {
    Server::bind(
        f.manager.clone(),
        p.clone(),
        Arc::new(SyntheticHost),
        Arc::new(SystemClock),
        options(),
    )
    .await
}
fn directory(paths: &RuntimePaths) {
    fs::create_dir(&paths.directory).unwrap();
    fs::set_permissions(&paths.directory, fs::Permissions::from_mode(0o700)).unwrap();
}

#[tokio::test]
async fn competing_owners_cannot_unlink_or_replace_the_live_socket() {
    let f = Fixture::new();
    let p = project(&f).await;
    let owner = bind(&f, &p).await.unwrap();
    let paths = owner.handle().paths().clone();
    let inode = fs::metadata(&paths.socket).unwrap().ino();
    assert_eq!(bind(&f, &p).await.err().unwrap().code, ErrorCode::Busy);
    assert_eq!(fs::metadata(&paths.socket).unwrap().ino(), inode);
    assert!(paths.socket.as_os_str().len() < 100);
    assert_eq!(
        fs::metadata(&paths.directory).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&paths.socket).unwrap().permissions().mode() & 0o777,
        0o600
    );
    drop(owner);
    assert!(!paths.socket.exists());
    // The lock file remains; only the held OS lock conveys ownership.
    assert!(paths.lock.exists());
    let successor = bind(&f, &p).await.unwrap();
    drop(successor);
}

#[tokio::test]
async fn stale_socket_is_reclaimed_but_an_active_unowned_endpoint_is_preserved() {
    let f = Fixture::new();
    let p = project(&f).await;
    let paths = RuntimePaths::for_project(&p);
    directory(&paths);
    let listener = UnixListener::bind(&paths.socket).unwrap();
    let inode = fs::metadata(&paths.socket).unwrap().ino();
    assert_eq!(bind(&f, &p).await.err().unwrap().code, ErrorCode::Busy);
    assert_eq!(fs::metadata(&paths.socket).unwrap().ino(), inode);
    drop(listener);
    let recovered = bind(&f, &p).await.unwrap();
    assert!(paths.socket.exists());
    drop(recovered);
    assert!(!paths.socket.exists());
}

#[tokio::test]
async fn foreign_files_symlinks_and_exposed_directories_are_not_repaired() {
    let f = Fixture::new();
    let p = project(&f).await;
    let paths = RuntimePaths::for_project(&p);
    directory(&paths);
    fs::write(&paths.socket, b"foreign data").unwrap();
    assert_eq!(
        bind(&f, &p).await.err().unwrap().code,
        ErrorCode::ConfigurationConflict
    );
    assert_eq!(fs::read(&paths.socket).unwrap(), b"foreign data");
    fs::remove_file(&paths.socket).unwrap();
    let foreign = f.root.path().join("foreign");
    fs::write(&foreign, b"preserve").unwrap();
    symlink(&foreign, &paths.socket).unwrap();
    assert_eq!(
        bind(&f, &p).await.err().unwrap().code,
        ErrorCode::ConfigurationConflict
    );
    assert!(
        fs::symlink_metadata(&paths.socket)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(&foreign).unwrap(), b"preserve");
    fs::remove_file(&paths.socket).unwrap();
    // Failed claims may remove an empty owned directory; recreate explicitly.
    if !paths.directory.exists() {
        directory(&paths);
    }
    fs::set_permissions(&paths.directory, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        bind(&f, &p).await.err().unwrap().code,
        ErrorCode::ConfigurationConflict
    );
    assert_eq!(
        fs::metadata(&paths.directory).unwrap().permissions().mode() & 0o777,
        0o755
    );
    fs::remove_dir(&paths.directory).unwrap();
}

#[tokio::test]
async fn dropping_an_owner_preserves_a_foreign_replacement_and_long_home_uses_short_paths() {
    let f = Fixture::new();
    let long = f.root.path().join("very-long-home-component-".repeat(7));
    let manager = agentchat_runtime::project::ProjectManager::new(long).unwrap();
    let p = manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap()
        .project;
    let server = Server::bind(
        manager,
        p,
        Arc::new(SyntheticHost),
        Arc::new(SystemClock),
        options(),
    )
    .await
    .unwrap();
    let paths = server.handle().paths().clone();
    assert!(paths.socket.as_os_str().len() < 100);
    fs::remove_file(&paths.socket).unwrap();
    fs::write(&paths.socket, b"foreign replacement").unwrap();
    drop(server);
    assert_eq!(fs::read(&paths.socket).unwrap(), b"foreign replacement");
    fs::remove_file(&paths.socket).unwrap();
    fs::remove_dir(&paths.directory).unwrap();
}

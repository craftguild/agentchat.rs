//! Fresh names never discover, migrate, repair or delete legacy project state.
mod support;
use agentchat_core::project::Scope;
use std::fs;
use support::Fixture;

#[tokio::test]
async fn both_reference_scopes_ignore_legacy_references_and_preserve_old_databases() {
    for scope in [Scope::Directory, Scope::Repository] {
        let f = Fixture::new();
        if scope == Scope::Repository {
            f.repository(&f.workspace);
        }
        let legacy_reference = if scope == Scope::Directory {
            f.workspace.join(".ac/project.json")
        } else {
            f.workspace.join(".git/ac/project.json")
        };
        fs::create_dir_all(legacy_reference.parent().unwrap()).unwrap();
        fs::write(&legacy_reference, b"legacy reference must remain untouched").unwrap();
        let old_database = f.root.path().join("old-state/retained/ac.sqlite3");
        fs::create_dir_all(old_database.parent().unwrap()).unwrap();
        fs::write(&old_database, b"legacy database sentinel").unwrap();
        assert!(f.manager.discover(&f.workspace).await.unwrap().is_none());
        let mut request = f.request();
        request.scope = Some(scope);
        let result = f.manager.initialize(&f.workspace, request).await.unwrap();
        let expected = if scope == Scope::Directory {
            f.workspace.join(".agentchat/project.json")
        } else {
            f.workspace.join(".git/agentchat/project.json")
        };
        assert_eq!(result.project.reference_path(), expected);
        assert_eq!(
            result.project.database_path().file_name().unwrap(),
            "agentchat.sqlite3"
        );
        assert!(result.project.database_path().starts_with(f.manager.home()));
        assert_eq!(
            f.manager.discover(&f.workspace).await.unwrap().unwrap(),
            result.project
        );
        assert_eq!(
            fs::read(&legacy_reference).unwrap(),
            b"legacy reference must remain untouched"
        );
        assert_eq!(
            fs::read(&old_database).unwrap(),
            b"legacy database sentinel"
        );
    }
}

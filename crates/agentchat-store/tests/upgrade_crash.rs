//! Process boundaries never turn an unsupported namespace into an implicit reset.
use agentchat_core::{
    ErrorCode,
    project::{ProjectIdentity, Scope},
};
use agentchat_store::{OpenProgress, OpenStage, Store, migrations::Migrator};
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database,
    sea_query::{Alias, Query},
};
use sea_orm_migration::MigratorTrait;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use tempfile::TempDir;
use uuid::Uuid;

struct Cut {
    point: String,
    log: PathBuf,
}
impl OpenProgress for Cut {
    fn reached(&self, stage: OpenStage) {
        let point = match stage {
            OpenStage::ReadOnlyChecked { .. } => "read_only_checked",
            OpenStage::BeforeCommit => "before_commit",
            OpenStage::Committed => "committed",
            _ => "other_schema_progress",
        };
        fs::write(&self.log, point).unwrap();
        if point == self.point {
            std::process::exit(86);
        }
    }
}

#[tokio::test]
#[ignore = "owned helper; parent invokes fresh bootstrap cuts and old-format refusal"]
async fn upgrade_child() {
    let path = PathBuf::from(std::env::var_os("AGENTCHAT_TEST_UPGRADE_DB").unwrap());
    let identity = ProjectIdentity {
        id: std::env::var("AGENTCHAT_TEST_UPGRADE_PROJECT")
            .unwrap()
            .parse()
            .unwrap(),
        scope: Scope::Directory,
    };
    let cut = Cut {
        point: std::env::var("AGENTCHAT_TEST_UPGRADE_CUT").unwrap(),
        log: PathBuf::from(std::env::var_os("AGENTCHAT_TEST_UPGRADE_LOG").unwrap()),
    };
    let result = Store::open_observed(path, identity, &cut).await;
    if cut.point == "refuse" {
        let error = result.err().unwrap();
        assert_eq!(error.code, ErrorCode::UnsupportedSchema);
        assert!(error.message.contains("--new-project"));
        return;
    }
    result.unwrap();
    panic!("Bootstrap did not reach selected cut");
}

struct Child(std::process::Child);
impl Child {
    fn run(
        path: &std::path::Path,
        identity: &ProjectIdentity,
        point: &str,
        log: &std::path::Path,
    ) -> std::process::ExitStatus {
        let mut child = Self(
            Command::new(std::env::current_exe().unwrap())
                .args(["--ignored", "--exact", "upgrade_child"])
                .env("AGENTCHAT_TEST_UPGRADE_DB", path)
                .env("AGENTCHAT_TEST_UPGRADE_PROJECT", identity.id.to_string())
                .env("AGENTCHAT_TEST_UPGRADE_CUT", point)
                .env("AGENTCHAT_TEST_UPGRADE_LOG", log)
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let until = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < until, "Setup child did not finish {point}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn fresh_bootstrap_crashes_retry_the_same_identity_without_partial_schema() {
    for point in ["read_only_checked", "before_commit", "committed"] {
        let root = TempDir::new().unwrap();
        let path = root.path().join("agentchat.sqlite3");
        let identity = ProjectIdentity {
            id: Uuid::new_v4(),
            scope: Scope::Directory,
        };
        let log = root.path().join("stage");
        assert_eq!(
            Child::run(&path, &identity, point, &log).code(),
            Some(86),
            "{point}"
        );
        assert_eq!(fs::read_to_string(log).unwrap(), point);
        for _ in 0..2 {
            let store = Store::open(&path, identity.clone()).await.unwrap();
            assert_eq!(store.identity(), &identity);
            store.integrity_check().await.unwrap();
            store.close().await.unwrap();
        }
        assert!(!fs::read_dir(root.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("backup")
        }));
    }
}

#[tokio::test]
async fn every_released_schema_child_refuses_before_any_schema_progress_or_reset() {
    for version in 1..agentchat_store::SCHEMA_VERSION {
        let root = TempDir::new().unwrap();
        let path = root.path().join("agentchat.sqlite3");
        let identity = ProjectIdentity {
            id: Uuid::new_v4(),
            scope: Scope::Directory,
        };
        let file = path.clone();
        let mut options = ConnectOptions::new("sqlite::memory:");
        options
            .max_connections(1)
            .sqlx_logging(false)
            .map_sqlx_sqlite_opts(move |o| {
                o.filename(&file)
                    .in_memory(false)
                    .create_if_missing(true)
                    .foreign_keys(false)
            });
        let db = Database::connect(options).await.unwrap();
        Migrator::up(&db, Some(version as u32)).await.unwrap();
        db.execute(
            &Query::insert()
                .into_table(Alias::new("project_meta"))
                .columns(
                    [
                        "singleton",
                        "project_id",
                        "scope",
                        "created_at",
                        "schema_version",
                        "format_version",
                    ]
                    .map(Alias::new),
                )
                .values_panic([
                    1.into(),
                    identity.id.to_string().into(),
                    "directory".into(),
                    "2026-10-31T00:00:00.000Z".into(),
                    version.into(),
                    1.into(),
                ])
                .to_owned(),
        )
        .await
        .unwrap();
        db.close().await.unwrap();
        let before = fs::read(&path).unwrap();
        let log = root.path().join("unexpected-progress");
        assert_eq!(Child::run(&path, &identity, "refuse", &log).code(), Some(0));
        assert!(
            !log.exists(),
            "old format reached schema progress: {version}"
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(!fs::read_dir(root.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("backup")
        }));
    }
}

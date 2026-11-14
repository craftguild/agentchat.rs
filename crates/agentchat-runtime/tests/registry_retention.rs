//! Real process exits, original owner GC and SeaORM initialization retry history.
//! Native identities are synthetic; these tests do not admit a host profile.
mod support;
use agentchat_core::{
    Error, ErrorCode, Result,
    model::{Context, HostKind},
    ports::Storage,
};
use agentchat_hosts::claude::{
    NativeLifecycle, NativeRegistration, metadata,
    namespace::{LifecycleOwner, OwnerNamespace, OwnerReaper, Reservation},
    observer::RegisteredOwner,
    process::{BirthState, ProcessIdentity},
    routing,
};
use agentchat_protocol::{
    operations::{Backend, Operation, Target},
    rpc::success,
};
use agentchat_runtime::{
    context::{ContextRegistry, ContextSeed},
    operations::{Launcher, Operations},
    project::{InitProgress, InitStage, Project, ProjectManager},
};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};
use support::Fixture;
use uuid::Uuid;

struct NoLaunch;
#[async_trait::async_trait]
impl Launcher for NoLaunch {
    async fn launch(&self, _: &Project, _: &Path) -> Result<()> {
        Err(Error::new(
            ErrorCode::SessionRequired,
            "This identity/history test must not launch a runtime.",
        ))
    }
}
struct ExitAt(Option<InitStage>);
impl InitProgress for ExitAt {
    fn reached(&self, stage: InitStage) -> Result<()> {
        if self.0 == Some(stage) {
            std::process::exit(86);
        }
        Ok(())
    }
}
fn seed(directory: &Path, root: &str) -> ContextSeed {
    ContextSeed {
        host: HostKind::Claude,
        host_session_id: root.into(),
        host_version: "2.1.301".into(),
        os_profile: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        working_directory: directory.into(),
    }
}
fn target(c: &Context) -> Target {
    Target {
        context_id: Some(c.context_id),
        expected_project_id: Some(c.project_id),
        ..Target::default()
    }
}
fn initialize(new_project: bool) -> Operation {
    Operation::Initialize {
        auto_read: None,
        delivery_confirmation_timeout_seconds: None,
        scope: None,
        relocate: false,
        new_project,
        default_channels: None,
    }
}
fn save(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
    fs::File::open(path).unwrap().sync_all().unwrap();
}
#[tokio::test]
#[ignore = "owned writer/init subprocess; parents exercise durable cuts explicitly"]
async fn registry_process_child() {
    assert_eq!(
        std::env::var_os("HOME"),
        std::env::var_os("AGENTCHAT_EXPECTED_HOME")
    );
    assert_eq!(
        std::env::var_os("CODEX_HOME"),
        std::env::var_os("AGENTCHAT_EXPECTED_CODEX_HOME")
    );
    let manager = ProjectManager::from_environment().unwrap();
    let directory = PathBuf::from(std::env::var_os("AGENTCHAT_REGISTRY_WORKSPACE").unwrap());
    let root = Uuid::parse_str(&std::env::var("AGENTCHAT_REGISTRY_ROOT").unwrap()).unwrap();
    let output = PathBuf::from(std::env::var_os("AGENTCHAT_REGISTRY_RESULT").unwrap());
    let registry = ContextRegistry::new(manager.clone());
    let mode = std::env::var("AGENTCHAT_REGISTRY_MODE").unwrap();
    if mode.starts_with("allocation") {
        let (owner, intent): (ProcessIdentity, Reservation) =
            serde_json::from_str(&std::env::var("AGENTCHAT_REGISTRY_ALLOCATION").unwrap()).unwrap();
        let ns = OwnerNamespace::existing(manager.home(), owner)
            .await
            .unwrap();
        let proposed = registry
            .prepare(seed(&directory, &root.to_string()))
            .await
            .unwrap();
        let context = ns.allocate_context(intent, proposed).await.unwrap();
        if mode == "allocation" {
            registry.publish(context.clone()).await.unwrap();
        }
        save(&output, &json!({"context":context}));
        // Durable allocation is complete; activation/commit never happens.
        std::process::exit(86);
    }
    let owner = ProcessIdentity::capture(std::process::id()).unwrap();
    let ns = OwnerNamespace::open(manager.home(), owner.clone())
        .await
        .unwrap();
    let intent = ns.reserve(root, directory.clone(), None).await.unwrap();
    let proposed = registry
        .prepare(seed(&directory, &root.to_string()))
        .await
        .unwrap();
    let context = ns.allocate_context(intent.clone(), proposed).await.unwrap();
    let context = registry.publish(context).await.unwrap();
    let registration = NativeRegistration {
        owner_root: ns.id(),
        observer: ns.observer().into(),
        generation: intent.generation,
        root,
        context: context.clone(),
    };
    ns.commit(intent, NativeLifecycle::Project(registration.clone()))
        .await
        .unwrap();
    ns.observation_sequence(root, registration.generation)
        .await
        .unwrap();
    ns.prepare_call(
        root,
        registration.generation,
        "mcp__agentchat__agentchat_status".into(),
        "pre_init_request".into(),
    )
    .await
    .unwrap();
    let native = RegisteredOwner {
        registration: registration.clone(),
        owner: owner.clone(),
    };
    metadata::write(
        &ns.directory()
            .join(format!("context-{}.json", context.context_id)),
        &native,
    )
    .unwrap();
    metadata::write(
        &ns.directory()
            .join(format!("native-{}.json", ns.observer())),
        &native,
    )
    .unwrap();
    metadata::write(
        &ns.directory()
            .join(format!("current-owner-{}.json", ns.id())),
        &LifecycleOwner {
            registration: NativeLifecycle::Project(registration),
            owner: owner.clone(),
        },
    )
    .unwrap();
    routing::publish(manager.home(), &native).unwrap();
    save(
        &output,
        &json!({"context":context,"owner":owner,"namespace":ns.directory()}),
    );
    let stage: Option<InitStage> =
        serde_json::from_str(&std::env::var("AGENTCHAT_REGISTRY_STAGE").unwrap()).unwrap();
    let id = Uuid::parse_str(&std::env::var("AGENTCHAT_REGISTRY_REQUEST").unwrap()).unwrap();
    let backend = Operations::new(
        manager.with_progress(Arc::new(ExitAt(stage))),
        Arc::new(NoLaunch),
    );
    let result = success(backend.invoke(id, target(&context), initialize(true)).await).unwrap();
    assert!(
        stage.is_none(),
        "Initialization missed its selected durable cut"
    );
    save(&output.with_extension("completed.json"), &result);
    std::process::exit(86);
}
struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
fn launch(
    f: &Fixture,
    mode: &str,
    root: Uuid,
    allocation: Option<(ProcessIdentity, Reservation)>,
    request: Uuid,
    stage: Option<InitStage>,
) -> Value {
    let output = f.root.path().join("child.json");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--ignored", "--exact", "registry_process_child"])
        .env("AGENTCHAT_HOME", f.manager.home())
        .env("AGENTCHAT_USER_CONFIG_ROOT", &f.config)
        .env_remove("AGENTCHAT_CONTEXT")
        .env("AGENTCHAT_REGISTRY_WORKSPACE", &f.workspace)
        .env("AGENTCHAT_REGISTRY_MODE", mode)
        .env("AGENTCHAT_REGISTRY_ROOT", root.to_string())
        .env("AGENTCHAT_REGISTRY_REQUEST", request.to_string())
        .env("AGENTCHAT_REGISTRY_RESULT", &output)
        .env(
            "AGENTCHAT_REGISTRY_STAGE",
            serde_json::to_string(&stage).unwrap(),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    if let Some(value) = allocation {
        command.env(
            "AGENTCHAT_REGISTRY_ALLOCATION",
            serde_json::to_string(&value).unwrap(),
        );
    }
    for (actual, expected) in [
        ("HOME", "AGENTCHAT_EXPECTED_HOME"),
        ("CODEX_HOME", "AGENTCHAT_EXPECTED_CODEX_HOME"),
    ] {
        if let Some(value) = std::env::var_os(actual) {
            command.env(expected, value);
        } else {
            command.env_remove(expected);
        }
    }
    let mut child = OwnedChild(command.spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(40);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "Owned registry child did not reach {stage:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(status.code(), Some(86), "{mode} / {stage:?}");
    serde_json::from_slice(&fs::read(output).unwrap()).unwrap()
}
#[tokio::test]
async fn killed_precommit_creator_reuses_the_same_full_descriptor_in_a_new_registry() {
    for mode in ["allocation-before-publication", "allocation"] {
        let f = Fixture::new();
        f.manager
            .initialize(&f.workspace, f.request())
            .await
            .unwrap();
        let root = Uuid::new_v4();
        let owner = ProcessIdentity::capture(std::process::id()).unwrap();
        let ns = OwnerNamespace::open(f.manager.home(), owner.clone())
            .await
            .unwrap();
        let intent = ns.reserve(root, f.workspace.clone(), None).await.unwrap();
        let observed = launch(
            &f,
            mode,
            root,
            Some((owner, intent.clone())),
            Uuid::new_v4(),
            None,
        );
        let old: Context = serde_json::from_value(observed["context"].clone()).unwrap();
        assert_eq!(
            f.manager.home().join("contexts").exists(),
            mode == "allocation"
        );
        let registry = ContextRegistry::new(ProjectManager::new(f.manager.home()).unwrap());
        let proposed = registry
            .prepare(seed(&f.workspace, &root.to_string()))
            .await
            .unwrap();
        let context = ns.allocate_context(intent.clone(), proposed).await.unwrap();
        assert_eq!(registry.publish(context).await.unwrap(), old);
        assert_eq!(registry.descriptor(old.context_id).await.unwrap(), old);
        assert_eq!(
            fs::read_dir(f.manager.home().join("contexts"))
                .unwrap()
                .count(),
            1
        );
        let r = NativeRegistration {
            owner_root: ns.id(),
            observer: ns.observer().into(),
            generation: intent.generation,
            root,
            context: old.clone(),
        };
        ns.commit(intent, NativeLifecycle::Project(r))
            .await
            .unwrap();
        assert!(!f.manager.home().join("hosts/claude/contexts").exists());
        assert!(fs::read_dir(&f.config).unwrap().next().is_none());
    }
}
fn collect(f: &Fixture, path: &Path) {
    let key = path.file_name().unwrap().to_str().unwrap();
    let parent = path.parent().unwrap();
    let retired = parent.join(format!(".retired-{key}"));
    let certificate = parent.join(format!(".gc-{key}.json"));
    let mut reaper = OwnerReaper::new(f.manager.home());
    let deadline = Instant::now() + Duration::from_secs(20);
    while path.exists() || retired.exists() || certificate.exists() {
        let report = reaper.step().unwrap();
        assert!(report.visited <= 8 && report.removed <= 40 && report.directories_removed <= 1);
        assert!(
            Instant::now() < deadline,
            "Original dead namespace did not finish collecting: {report:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
#[tokio::test]
async fn successful_and_every_pending_init_cut_replay_after_dead_owner_collection_without_retargeting()
 {
    for stage in [
        None,
        Some(InitStage::Prepared),
        Some(InitStage::DatabaseInitialized),
        Some(InitStage::BindingPublished),
        Some(InitStage::ReferencePublished { index: 0 }),
        Some(InitStage::ExclusionsUpdated),
        Some(InitStage::ReceiptPublished),
    ] {
        let f = Fixture::new();
        let first = f
            .manager
            .initialize(&f.workspace, f.request())
            .await
            .unwrap();
        let request = Uuid::new_v4();
        let root = Uuid::new_v4();
        let observed = launch(&f, "initialization", root, None, request, stage);
        let context: Context = serde_json::from_value(observed["context"].clone()).unwrap();
        let owner: ProcessIdentity = serde_json::from_value(observed["owner"].clone()).unwrap();
        let namespace = PathBuf::from(observed["namespace"].as_str().unwrap());
        assert_eq!(owner.birth_state(), BirthState::Gone);
        let descriptor = f
            .manager
            .home()
            .join("contexts")
            .join(format!("{}.json", context.context_id));
        let bytes = fs::read(&descriptor).unwrap();
        let inode = fs::metadata(&descriptor).unwrap().ino();
        let signal = f.workspace.join(".agentchat-wakeup");
        let signal_bytes = fs::read(&signal).unwrap();
        let signal_inode = fs::metadata(&signal).unwrap().ino();
        let pending = f.manager.home().join("init/pending.json");
        let pending_bytes = fs::read(&pending).ok();
        let receipt = f
            .manager
            .home()
            .join("init/requests")
            .join(format!("{request}.json"));
        let completed_bytes = fs::read(&receipt).ok();
        let intended: Uuid = if let Some(bytes) = &pending_bytes {
            serde_json::from_slice::<Value>(bytes).unwrap()["receipt"]["binding"]["identity"]["id"]
                .as_str()
                .unwrap()
                .parse()
                .unwrap()
        } else {
            serde_json::from_slice::<Value>(
                &fs::read(f.root.path().join("child.completed.json")).unwrap(),
            )
            .unwrap()["project_id"]
                .as_str()
                .unwrap()
                .parse()
                .unwrap()
        };
        collect(&f, &namespace);
        assert_eq!(fs::read(&descriptor).unwrap(), bytes);
        assert_eq!(fs::metadata(&descriptor).unwrap().ino(), inode);
        assert_eq!(fs::read(&pending).ok(), pending_bytes);
        assert_eq!(fs::read(&receipt).ok(), completed_bytes);
        assert_eq!(fs::read(&signal).unwrap(), signal_bytes);
        assert_eq!(fs::metadata(&signal).unwrap().ino(), signal_inode);
        assert!(
            !f.manager
                .home()
                .join("hosts/claude/contexts")
                .join(format!("{}.json", context.context_id))
                .exists()
        );
        let backend = Operations::new(
            ProjectManager::new(f.manager.home()).unwrap(),
            Arc::new(NoLaunch),
        );
        let original = success(
            backend
                .invoke(request, target(&context), initialize(true))
                .await,
        )
        .unwrap();
        assert_eq!(original["project_id"], intended.to_string());
        assert_eq!(original["created"], true);
        assert!(!pending.exists());
        assert_eq!(
            success(
                backend
                    .invoke(request, target(&context), initialize(true))
                    .await
            )
            .unwrap(),
            original
        );
        assert_eq!(
            f.manager
                .discover(&f.workspace)
                .await
                .unwrap()
                .unwrap()
                .identity()
                .id,
            intended
        );
        let mut later = f.request();
        later.new_project = true;
        let latest = f.manager.initialize(&f.workspace, later).await.unwrap();
        let reference = fs::read(latest.project.reference_path()).unwrap();
        // Successful/pending retry history is read before current routing. A
        // retained identity must never cause an old project to be republished.
        assert_eq!(
            success(
                backend
                    .invoke(request, target(&context), initialize(true))
                    .await
            )
            .unwrap(),
            original
        );
        let registry = ContextRegistry::new(f.manager.clone());
        assert_eq!(
            registry
                .resolve(context.context_id, None)
                .await
                .unwrap_err()
                .code,
            ErrorCode::ProjectBindingMismatch
        );
        assert_eq!(
            success(
                backend
                    .invoke(Uuid::new_v4(), target(&context), initialize(true))
                    .await
            )
            .unwrap_err()
            .code,
            ErrorCode::ProjectBindingMismatch
        );
        assert_eq!(
            success(
                backend
                    .invoke(request, target(&context), initialize(false))
                    .await
            )
            .unwrap_err()
            .code,
            ErrorCode::IdempotencyConflict
        );
        let different = registry
            .issue(seed(&f.workspace, &Uuid::new_v4().to_string()))
            .await
            .unwrap();
        assert_eq!(
            success(
                backend
                    .invoke(request, target(&different), initialize(true))
                    .await
            )
            .unwrap_err()
            .code,
            ErrorCode::IdempotencyConflict
        );
        let resumed = registry
            .issue(seed(&f.workspace, &root.to_string()))
            .await
            .unwrap();
        assert_ne!(resumed.context_id, context.context_id);
        assert_ne!(resumed.incarnation, context.incarnation);
        assert_eq!(
            success(
                backend
                    .invoke(request, target(&resumed), initialize(true))
                    .await
            )
            .unwrap(),
            original
        );
        assert_eq!(
            fs::read(latest.project.reference_path()).unwrap(),
            reference
        );
        for identity in [
            first.project.identity().clone(),
            agentchat_core::project::ProjectIdentity {
                id: intended,
                scope: first.project.identity().scope,
            },
            latest.project.identity().clone(),
        ] {
            let store = agentchat_store::Store::open(
                f.manager
                    .home()
                    .join(identity.id.to_string())
                    .join("agentchat.sqlite3"),
                identity,
            )
            .await
            .unwrap();
            store.integrity_check().await.unwrap();
            let mut snapshot = store.snapshot().await.unwrap();
            assert_eq!(snapshot.head().get(), 2);
            assert_eq!(
                snapshot
                    .channel_policy()
                    .await
                    .unwrap()
                    .unwrap()
                    .ordered_defaults(),
                &["dev".to_owned()]
            );
            let stats = snapshot.statistics().await.unwrap();
            assert_eq!(stats.agents, 0);
            assert_eq!(stats.messages, 0);
            drop(snapshot);
            store.close().await.unwrap();
        }
        assert!(fs::read_dir(&f.config).unwrap().next().is_none());
    }
}

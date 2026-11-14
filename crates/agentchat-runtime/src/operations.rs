//! Public operation composition shared by CLI and the MCP backend port.

mod activation;
mod actor;
mod attachment;
mod init;
mod managed_supervision;
mod native_codex;
mod native_launch;
mod native_supervisor;
mod removal;
mod viewer;
pub use native_launch::NativeLaunch;
pub use viewer::ViewerSession;

use crate::{
    context::ContextRegistry,
    daemon::{RuntimePaths, connect_or_start},
    project::{Project, ProjectManager},
};
use agentchat_core::{
    Error, ErrorCode, Result,
    application::Application,
    model::Context,
    read::{Query, QueryResult},
    time::SystemClock,
};
use agentchat_protocol::{
    envelope::Envelope,
    operations::{Attachment, Backend, Operation, Target},
    rpc::{Call, Client, success},
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use uuid::Uuid;

/// Executable startup is supplied by the composition root, not business policy.
#[async_trait]
pub trait Launcher: Send + Sync {
    /// Start an owner for this exact namespace; losing an OS-lock race is safe.
    async fn launch(&self, project: &Project, directory: &Path) -> Result<()>;
}
/// One public operation service, independent of CLI/MCP argument syntax.
#[derive(Clone)]
pub struct Operations {
    manager: ProjectManager,
    launcher: Arc<dyn Launcher>,
    host_tools: Option<agentchat_hosts::setup::ToolSetup>,
}
/// Captured routing with exact descriptor and explicit discovery directory.
pub struct Route {
    /// Validated initialized namespace.
    pub project: Project,
    /// Original canonical routing directory, usable by a child daemon.
    pub directory: PathBuf,
    /// Optional exact conversation; never inferred from directory alone.
    pub context: Option<Context>,
    /// Explicit anonymous selection; never inferred from an absent context.
    pub user: bool,
}
impl Operations {
    /// Compose project policy with the executable launcher.
    pub fn new(manager: ProjectManager, launcher: Arc<dyn Launcher>) -> Self {
        Self {
            manager,
            launcher,
            host_tools: None,
        }
    }
    /// Compose ownership-preserving tool installation for both public transports.
    /// Managed Codex launch can prepare its missing native selector through this
    /// installer. Configuration alone grants no conversation/lifetime authority.
    pub fn with_host_tools(mut self, setup: agentchat_hosts::setup::ToolSetup) -> Self {
        self.host_tools = Some(setup);
        self
    }
    /// Resolve explicit inputs and reject any context/project disagreement.
    pub async fn route(&self, target: &Target) -> Result<Route> {
        if target.actor.is_some() && target.context_id.is_some() {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Public actor selection cannot be combined with a private native context.",
            ));
        }
        if let Some(actor) = &target.actor {
            actor.validate()?;
        }
        let mut context = if let Some(id) = target.context_id {
            Some(
                ContextRegistry::new(self.manager.clone())
                    .resolve(id, None)
                    .await?,
            )
        } else {
            None
        };
        let directory = target
            .project_path
            .clone()
            .or_else(|| context.as_ref().map(|c| c.working_directory.clone()))
            .ok_or_else(|| {
                Error::new(
                    ErrorCode::InvalidArgument,
                    "An unbound operation requires an explicit project path.",
                )
            })?;
        let project = self.manager.discover(&directory).await?.ok_or_else(|| {
            Error::new(
                ErrorCode::NotInitialized,
                "Project is not initialized; run agentchat install explicitly.",
            )
        })?;
        if context
            .as_ref()
            .is_some_and(|c| c.project_id != project.identity().id)
        {
            return Err(Error::new(
                ErrorCode::ProjectBindingMismatch,
                "Explicit project conflicts with the exact host descriptor.",
            ));
        }
        if target
            .expected_project_id
            .is_some_and(|expected| expected != project.identity().id)
        {
            return Err(Error::new(
                ErrorCode::ProjectBindingMismatch,
                "Bound transport cannot switch its captured project.",
            ));
        }
        if let Some(selected @ agentchat_core::model::ActorSelector::Native { .. }) = &target.actor
        {
            context = Some(self.select_native(project.identity().id, selected).await?);
        }
        Ok(Route {
            project,
            directory,
            context,
            user: matches!(
                target.actor,
                Some(agentchat_core::model::ActorSelector::User)
            ),
        })
    }
    /// Connect through checked lazy startup; useful for finite calls and watch.
    pub async fn connect(&self, route: &Route) -> Result<Client> {
        self.manager.validate(&route.project).await?;
        let paths = RuntimePaths::for_project(&route.project);
        connect_or_start(&paths, route.project.identity().id, || {
            self.launcher.launch(&route.project, &route.directory)
        })
        .await
    }
    /// Shared typed read composition, retaining the DTO until presentation.
    pub async fn read(
        &self,
        id: Uuid,
        target: Target,
        query: Query,
    ) -> Envelope<QueryResult<agentchat_protocol::health::PublicWhoami>> {
        let mut project = None;
        let result = async {
            let route = self.route(&target).await?;
            project = Some(route.project.identity().id);
            self.read_route(id, &route, query).await
        }
        .await;
        Envelope::new(id, project, result)
    }
    async fn read_route(
        &self,
        id: Uuid,
        route: &Route,
        query: Query,
    ) -> Result<QueryResult<agentchat_protocol::health::PublicWhoami>> {
        let mut client = self.connect(route).await?;
        let result = success(
            client
                .query_actor(id, route.actor_reference(), query)
                .await?,
        )?;
        if let QueryResult::Whoami(whoami) = result {
            return Ok(QueryResult::Whoami(
                agentchat_protocol::health::PublicWhoami {
                    whoami,
                    capabilities: agentchat_hosts::capability_report(),
                    integrations: self.integration_health().await,
                },
            ));
        }
        Ok(result.map_whoami(|_| unreachable!("whoami handled above")))
    }
    async fn execute(
        &self,
        id: Uuid,
        target: Target,
        operation: Operation,
        project_id: &mut Option<Uuid>,
    ) -> Result<Value> {
        if let Operation::Initialize {
            scope,
            default_channels,
            auto_read,
            delivery_confirmation_timeout_seconds,
            relocate,
            new_project,
        } = operation
        {
            // Retain one fence across both durable project setup and host
            // publication, including exact successful request replay.
            let _fence = self.manager.installation_fence().await?;
            let result = self
                .initialize(
                    target,
                    crate::project::InitRequest {
                        scope,
                        default_channels,
                        receipt_settings: agentchat_core::project::ReceiptSettingsUpdate {
                            auto_read,
                            delivery_confirmation_timeout_seconds,
                        },
                        relocate,
                        new_project,
                        ..crate::project::InitRequest::new(id)
                    },
                )
                .await?;
            *project_id = Some(result.project.identity().id);
            return self.install_integrations(id, &result).await;
        }
        let route = self.route(&target).await?;
        // Doctor opens storage directly rather than using an owned daemon.
        // Keep the finite operation quiescence-visible through its final result.
        let _activity = if matches!(operation, Operation::Doctor) {
            Some(self.manager.activity(route.project.identity().id).await?)
        } else {
            None
        };
        *project_id = Some(route.project.identity().id);
        let actor = route.actor_reference();
        match operation {
            Operation::Mutate { command } => {
                if !route.user
                    && matches!(command, agentchat_core::application::Mutation::Join { .. })
                {
                    return self.explicit_join(&route, id, command).await;
                }
                let actor = actor.ok_or_else(|| {
                    Error::new(
                        ErrorCode::SessionRequired,
                        "This mutation requires an explicit native or anonymous actor.",
                    )
                })?;
                let mut client = self.connect(&route).await?;
                success(
                    client
                        .call(
                            id,
                            Call::Mutate {
                                request_id: id,
                                actor,
                                command,
                            },
                        )
                        .await?,
                )
            }
            Operation::Query { query } => {
                let result = self.read_route(id, &route, query).await?;
                serde_json::to_value(result).map_err(|_| {
                    Error::new(
                        ErrorCode::IntegrityError,
                        "Read presentation serialization failed.",
                    )
                })
            }
            Operation::Doctor => {
                let store = self.manager.open(&route.project).await?;
                store.integrity_check().await?;
                let sqlite = store.sqlite_version().await?;
                let settings = store.connection_settings().await?;
                let app = Application::new(
                    store,
                    Arc::new(SystemClock),
                    Arc::new(agentchat_hosts::AdmittedHosts::new(
                        self.manager.home().to_owned(),
                    )),
                );
                let health = app.health().await?;
                let paths = RuntimePaths::for_project(&route.project);
                let runtime = match Client::connect(&paths.socket, route.project.identity().id)
                    .await
                {
                    Ok(mut client) => {
                        let instance = client.instance_id();
                        match client.health().await {
                            Ok(status) => {
                                json!({"reachable":true,"instance_id":instance,"diagnostics":status.runtime.diagnostics})
                            }
                            Err(error) => {
                                json!({"reachable":true,"instance_id":instance,"error":error})
                            }
                        }
                    }
                    Err(error) => json!({"reachable":false,"error":error}),
                };
                let logs = if let Some(health) = runtime.get("diagnostics") {
                    health.clone()
                } else {
                    match crate::diagnostics::inspect(
                        route.project.data_dir(),
                        route.project.identity().id,
                    )
                    .await
                    {
                        Ok(health) => health,
                        Err(error) => json!({"state":"invalid","error":error}),
                    }
                };
                let home = self.manager.home().to_owned();
                let staging = tokio::task::spawn_blocking(move || {
                    agentchat_hosts::staging::OriginReaper::new(home).inspect()
                })
                .await;
                let staging = match staging {
                    Ok(Ok(page)) => {
                        json!({"state":"observed","scope":"first_pending_index_page","visit_limit":8,"page":page})
                    }
                    _ => {
                        json!({"state":"unavailable","scope":"first_pending_index_page","visit_limit":8})
                    }
                };
                Ok(
                    json!({"project_id":route.project.identity().id,"database":{"integrity":"ok","sqlite_version":sqlite,"settings":settings},
                    "health":health,"runtime":runtime,"diagnostic_logs":logs,"pending_staging":staging,"hosts":agentchat_hosts::inspect().await,"capabilities":agentchat_hosts::capabilities(),"integrations":self.integration_status().await}),
                )
            }
            Operation::Initialize { .. } => {
                unreachable!("initialization is handled before initialized routing")
            }
        }
    }
}
#[async_trait]
impl Backend for Operations {
    async fn attach(&self, target: Target) -> Result<Box<dyn Attachment>> {
        self.attachment(target).await
    }
    async fn invoke(
        &self,
        request_id: Uuid,
        target: Target,
        operation: Operation,
    ) -> Envelope<Value> {
        let mut project = None;
        let result = self
            .execute(request_id, target, operation, &mut project)
            .await;
        Envelope::new(request_id, project, result)
    }
}

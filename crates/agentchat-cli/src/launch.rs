//! Interactive argument/terminal adaptation; startup policy lives in Operations.
use crate::{
    args::{Cli, Command},
    process,
};
use agentchat_core::{Error, ErrorCode, Result};
use agentchat_protocol::{envelope::Envelope, operations::Target};
use agentchat_runtime::{operations::NativeLaunch, project::ProjectManager};
use uuid::Uuid;

pub(crate) async fn run(cli: Cli, generated: Uuid) -> i32 {
    let json = cli.json;
    let request = cli.request_id.unwrap_or(generated);
    let mut project = None;
    match execute(cli, &mut project).await {
        Ok(code) => {
            if json {
                let result = Envelope::new(
                    request,
                    project,
                    Ok(serde_json::json!({"native_exit_code":code})),
                );
                if crate::output(&result, true).await != 0 {
                    return 8;
                }
            }
            code
        }
        Err(error) => crate::output(&Envelope::new(request, project, Err(error)), json).await,
    }
}
async fn execute(cli: Cli, project: &mut Option<Uuid>) -> Result<i32> {
    if cli.request_id.is_some() {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "Interactive native launch does not accept mutation retry options.",
        ));
    }
    let Command::Launch { host, resume, name } = cli.command else {
        unreachable!("launch dispatcher")
    };
    // Namespace discovery uses the explicit project/cwd. An inherited or supplied
    // context never selects the new native actor or lends it old authority.
    let directory = match cli.project {
        Some(path) => path,
        None => std::env::current_dir().map_err(|_| {
            Error::new(
                ErrorCode::IoError,
                "Current directory is unavailable; pass --project.",
            )
        })?,
    };
    let target = Target {
        project_path: Some(directory),
        ..Target::default()
    };
    let manager = ProjectManager::from_environment()?;
    let operations = process::operations(manager, process::current_executable()?);
    let route = operations.route(&target).await?;
    *project = Some(route.project.identity().id);
    operations
        .launch_native(
            &route,
            NativeLaunch {
                host: host.into(),
                resume,
                name,
                terminal: if cli.json {
                    agentchat_hosts::launch::NativeTerminal::Controlling
                } else {
                    agentchat_hosts::launch::NativeTerminal::Inherited
                },
            },
        )
        .await
}

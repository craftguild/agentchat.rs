//! Human viewer composition: initialized project discovery without agent context.

use crate::{args::Cli, process};
use agentchat_core::{Error, ErrorCode, Result};
use agentchat_protocol::envelope::Envelope;
use agentchat_runtime::project::ProjectManager;
use agentchat_web::{Viewer, ViewerContext};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

pub(crate) async fn run(cli: Cli, generated: Uuid) -> i32 {
    let id = cli.request_id.unwrap_or(generated);
    let mut project = None;
    match serve(&cli, id, &mut project).await {
        Ok(()) => 0,
        Err(error) => crate::output(&Envelope::new(id, project, Err(error)), cli.json).await,
    }
}
async fn serve(cli: &Cli, id: Uuid, project: &mut Option<Uuid>) -> Result<()> {
    let directory = cli
        .project
        .clone()
        .map(Ok)
        .unwrap_or_else(std::env::current_dir)
        .map_err(|_| {
            Error::new(
                ErrorCode::IoError,
                "Current directory is unavailable; pass --project explicitly.",
            )
        })?;
    let operations = process::operations(
        ProjectManager::from_environment()?,
        process::current_executable()?,
    );
    // The global/inherited conversation descriptor is deliberately never passed
    // to the viewer. Project discovery is the only cwd-based inference here.
    let session = operations.viewer(directory).await?;
    let display = ViewerContext::from_directory(session.directory());
    let reader = session.reader();
    *project = Some(reader.project_id());
    let viewer = Viewer::bind_with_writer(reader, Some(session.writer()), display).await?;
    if cli.json {
        let ready = Envelope::new(id, *project, Ok(serde_json::json!({ "url": viewer.url() })));
        if crate::output(&ready, true).await != 0 {
            return Err(Error::new(
                ErrorCode::IoError,
                "Could not write viewer ready URL.",
            ));
        }
    } else {
        let mut output = tokio::io::stdout();
        output
            .write_all(format!("{}\n", viewer.url()).as_bytes())
            .await
            .map_err(|_| Error::new(ErrorCode::IoError, "Could not write viewer ready URL."))?;
        output
            .flush()
            .await
            .map_err(|_| Error::new(ErrorCode::IoError, "Could not flush viewer ready URL."))?;
    }
    let result = viewer
        .serve(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await;
    session.close().await;
    result
}

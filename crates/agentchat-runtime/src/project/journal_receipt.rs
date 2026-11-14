//! Typed initialization outcome recovery from a durable completed journal.

use super::{InitOutcome, ProjectManager, journal::Completed};
use agentchat_core::{Result, project::ProjectChannelPolicy};

impl Completed {
    pub fn outcome(&self, manager: &ProjectManager) -> Result<InitOutcome> {
        let channel_policy = self.default_channels.clone().map_or_else(
            || Ok(ProjectChannelPolicy::legacy()),
            ProjectChannelPolicy::new,
        )?;
        Ok(InitOutcome {
            project: self.receipt.binding.project(manager)?,
            created: self.receipt.created,
            channel_policy,
            receipt_settings: self.receipt_settings,
        })
    }
}

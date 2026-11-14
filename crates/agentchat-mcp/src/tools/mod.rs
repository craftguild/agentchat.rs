//! Named, typed tools that only adapt inputs to shared application operations.

use crate::{Server, params, result};
use agentchat_core::{
    application::Mutation,
    read::{PageRequest, Query},
};
use agentchat_protocol::operations::Operation;
use rmcp::{handler::server::wrapper::Parameters, model::CallToolResult, tool, tool_router};
mod identity;
mod messages;
mod project;
mod reservations;

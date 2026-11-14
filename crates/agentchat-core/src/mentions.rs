//! Source-aware Markdown candidate extraction and transactional recipient resolution.

use std::collections::{BTreeMap, BTreeSet};

use crate::{Error, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

mod image_alt;
mod lexer;
mod presentation;
pub use presentation::{
    MentionOccurrence, MentionText, RegionMention, markdown_features, occurrences, parse,
    presentation,
};

/// Bound detailed outcomes and complete event/result envelopes for one message.
pub const MAX_INDIVIDUAL_MENTIONS: usize = 200;

/// Snapshot needed to resolve mentions inside the serialized write transaction.
#[derive(Debug, Clone)]
pub struct MentionParticipant {
    /// Project-specific participant identity.
    pub id: Uuid,
    /// Normalized immutable handle.
    pub handle: String,
    /// Whether the participant is currently joined.
    pub joined: bool,
    /// Whether the participant currently belongs to the message channel.
    pub in_channel: bool,
    /// Valid host-conversation liveness, including paused/approval-blocked sessions.
    pub connected: bool,
    /// Live host controls currently defer native wake presentation.
    pub wake_deferred: bool,
}

/// Frozen diagnostic state for one explicitly named individual.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MentionState {
    /// No retained identity owns this handle.
    Unknown,
    /// The retained identity explicitly left the project.
    Left,
    /// Joined to the project but not to this message channel.
    NotInChannel,
    /// Joined, with no current valid live connection.
    Offline,
    /// Joined and connected, but pause/approval controls defer wake.
    Deferred,
    /// Joined and eligible at commit time; this is not model receipt.
    Queued,
    /// The author named itself; never a wake or read-watch recipient.
    #[serde(rename = "self")]
    SelfMention,
}
/// Public resolution result, retained alongside the successful message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndividualMention {
    /// Complete normalized handle from eligible authored Markdown.
    pub handle: String,
    /// Retained identity, absent for an unresolved name.
    pub recipient_id: Option<Uuid>,
    /// Availability at commit, unaffected by later presence changes.
    pub state: MentionState,
}
/// Successful-post warning; it must not trigger a duplicate transport retry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MentionWarning {
    /// Stable code: unknown_recipient, recipient_left/offline/deferred.
    pub code: String,
    /// Normalized explicit handle.
    pub handle: String,
    /// Resolved retained identity, if any.
    pub recipient_id: Option<Uuid>,
}
/// Atomically frozen wake selections and individual diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionResolution {
    /// Deduplicated eligible identities and every mention source selecting them.
    pub recipients: BTreeMap<Uuid, BTreeSet<MentionSource>>,
    /// Explicit individual outcomes in normalized handle order.
    pub individuals: Vec<IndividualMention>,
    /// Actionable diagnostics, in the same order as individual outcomes.
    pub warnings: Vec<MentionWarning>,
}

/// Source of a frozen wake recipient selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MentionSource {
    /// An explicit individual handle.
    Individual,
    /// All members of the channel.
    Channel,
    /// All project participants.
    All,
    /// Connected participants at commit time.
    Here,
}

/// Parse complete, case-normalized mention tokens, deduplicated and sorted.
pub fn tokens(markdown: &str) -> Result<BTreeSet<String>> {
    let parsed = parse(markdown)?;
    delivery_tokens(&parsed)
}

/// Collect delivery candidates, including authored image alt without inventing
/// display regions. This never resolves membership or grants recipient authority.
pub fn delivery_tokens(parsed: &oriha_core::ParsedDocument) -> Result<BTreeSet<String>> {
    let mut handles = occurrences(parsed)?
        .into_iter()
        .map(|item| item.handle)
        .collect::<BTreeSet<_>>();
    handles.extend(image_alt::tokens(parsed.document()));
    Ok(handles)
}

/// Freeze mentioned recipients from a transaction's participation/presence snapshot.
///
/// Supply joined channel members and any explicitly named retained left users.
/// Unknown names remain diagnostics, never invented or later-retargeted users.
/// A transport must never resolve these again after commit.
pub fn resolve(
    markdown: &str,
    sender: Uuid,
    participants: &[MentionParticipant],
) -> Result<MentionResolution> {
    Ok(resolve_tokens(
        bounded_tokens(markdown)?,
        sender,
        participants,
    ))
}

// Extraction is pure and reusable; authority is still frozen from the current
// transaction snapshot. Do not parse the body again after the per-handle reads.
pub(crate) fn resolve_tokens(
    tokens: BTreeSet<String>,
    sender: Uuid,
    participants: &[MentionParticipant],
) -> MentionResolution {
    let mut recipients = BTreeMap::<Uuid, BTreeSet<MentionSource>>::new();
    let mut individuals = Vec::new();
    let mut warnings = Vec::new();
    for token in tokens {
        let source = match token.as_str() {
            "channel" => MentionSource::Channel,
            "all" => MentionSource::All,
            "here" => MentionSource::Here,
            _ => MentionSource::Individual,
        };
        if source == MentionSource::Individual {
            let participant = participants.iter().find(|p| p.handle == token);
            let state = match participant {
                None => MentionState::Unknown,
                Some(p) if !p.joined => MentionState::Left,
                Some(p) if !p.in_channel => MentionState::NotInChannel,
                Some(p) if p.id == sender => MentionState::SelfMention,
                Some(p) if !p.connected => MentionState::Offline,
                Some(p) if p.wake_deferred => MentionState::Deferred,
                Some(_) => MentionState::Queued,
            };
            let recipient_id = participant.map(|p| p.id);
            let code = match state {
                MentionState::Unknown => Some("unknown_recipient"),
                MentionState::Left => Some("recipient_left"),
                MentionState::NotInChannel => Some("not_in_channel"),
                MentionState::Offline => Some("recipient_offline"),
                MentionState::Deferred => Some("recipient_deferred"),
                MentionState::Queued | MentionState::SelfMention => None,
            };
            if let Some(code) = code {
                warnings.push(MentionWarning {
                    code: code.into(),
                    handle: token.clone(),
                    recipient_id,
                });
            }
            individuals.push(IndividualMention {
                handle: token.clone(),
                recipient_id,
                state,
            });
        }
        for p in participants
            .iter()
            .filter(|p| p.joined && p.in_channel && p.id != sender)
        {
            if match source {
                MentionSource::Individual => p.handle == token,
                MentionSource::Here => p.connected,
                MentionSource::All | MentionSource::Channel => true,
            } {
                recipients.entry(p.id).or_default().insert(source);
            }
        }
    }
    MentionResolution {
        recipients,
        individuals,
        warnings,
    }
}

/// Validate the declared bound before performing per-handle storage lookups.
pub(crate) fn bounded_tokens(markdown: &str) -> Result<BTreeSet<String>> {
    let tokens = tokens(markdown)?;
    if tokens
        .iter()
        .filter(|name| !matches!(name.as_str(), "all" | "channel" | "here"))
        .count()
        > MAX_INDIVIDUAL_MENTIONS
    {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "A message supports at most 200 distinct individual mentions.",
        ));
    }
    Ok(tokens)
}

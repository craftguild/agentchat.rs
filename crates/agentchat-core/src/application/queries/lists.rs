//! Bounded ordered identity/channel/event/lease projections.

use super::{agent_view, channel_id};
use crate::{
    Error, ErrorCode, Result,
    model::{Event, LeaseState},
    read::{
        AgentView, ChannelListCondition, ChannelPage, ChannelView, Page, PageBuilder, PageRequest,
        ReservationView, Snapshot,
    },
    time::Timestamp,
};
use uuid::Uuid;

pub(super) async fn agents(
    db: &mut dyn Snapshot,
    now: Timestamp,
    input: PageRequest,
    channel: Option<Uuid>,
) -> Result<Page<AgentView>> {
    let mut page = PageBuilder::new(input, db.head())?;
    for row in db.agents(input.after, input.limit()? + 1, channel).await? {
        if !page.push(row.key.cursor(), agent_view(db, row.item, now).await?)? {
            break;
        }
    }
    Ok(page.finish())
}
const ARCHIVE_SECONDS: i64 = 2_592_000;

pub(super) async fn channels(
    db: &mut dyn Snapshot,
    now: Timestamp,
    condition: ChannelListCondition,
    input: PageRequest,
) -> Result<ChannelPage> {
    let cutoff = now.after_seconds(-ARCHIVE_SECONDS)?;
    let policy = db
        .channel_policy()
        .await?
        .unwrap_or_else(crate::project::ProjectChannelPolicy::legacy);
    let mut page = PageBuilder::new(input, db.head())?;
    for row in db
        .channels(input.after, input.limit()? + 1, condition, cutoff)
        .await?
    {
        let archive_at = row.last_conversation_at.after_seconds(ARCHIVE_SECONDS)?;
        let view = ChannelView {
            mandatory: policy.contains(&row.channel.name),
            archived: row.last_conversation_at <= cutoff,
            last_conversation_at: row.last_conversation_at,
            archive_at,
            channel: row.channel,
        };
        if !page.push(view.channel.creation_seq.cursor(), view)? {
            break;
        }
    }
    Ok(ChannelPage {
        page: page.finish(),
        as_of: now,
    })
}
pub(super) async fn events(
    db: &mut dyn Snapshot,
    allowed: Option<&[Uuid]>,
    channel: Option<String>,
    input: PageRequest,
) -> Result<Page<Event>> {
    let selected = if let Some(channel) = channel {
        let channel = channel_id(db, &channel).await?;
        if allowed.is_some_and(|channels| !channels.contains(&channel)) {
            return Err(Error::new(
                ErrorCode::NotJoined,
                "Join this channel before reading its events.",
            ));
        }
        Some(vec![channel])
    } else {
        allowed.map(<[Uuid]>::to_vec)
    };
    let mut page = PageBuilder::new(input, db.head())?;
    for row in db
        .events(input.after, input.limit()? + 1, selected.as_deref())
        .await?
    {
        if !page.push(row.seq.cursor(), row)? {
            break;
        }
    }
    Ok(page.finish())
}
pub(super) async fn reservations(
    db: &mut dyn Snapshot,
    now: Timestamp,
    owner: Option<String>,
    all: bool,
    input: PageRequest,
) -> Result<Page<ReservationView>> {
    let owners = if let Some(owner) = owner {
        let mut ids = Vec::new();
        if let Some(agent) = db.handle(&owner).await? {
            ids.push(agent.id);
        }
        if owner == "user"
            && let Some(user) = db.user().await?
        {
            ids.push(user.actor_id);
        }
        if ids.is_empty() {
            return Err(Error::new(
                ErrorCode::NotFound,
                "Reservation owner does not exist.",
            ));
        }
        Some(ids)
    } else {
        None
    };
    let mut page = PageBuilder::new(input, db.head())?;
    for mut group in db
        .reservations(owners, all, now, input.after, input.limit()? + 1)
        .await?
    {
        // Deadlines define effective ownership even before a writer materializes
        // the expiry event. This projection never mutates the retained row.
        if group.state == LeaseState::Active && group.expires_at <= now {
            group.state = LeaseState::Expired;
        }
        let key = crate::sequence::Sequence::new(group.generation)?.cursor();
        let owner = db.actor(group.owner_id).await?.ok_or_else(|| {
            Error::new(ErrorCode::IntegrityError, "Reservation owner is missing.")
        })?;
        if !page.push(key, ReservationView::with_owner(group, (&owner).into()))? {
            break;
        }
    }
    Ok(page.finish())
}

pub(super) async fn actors(
    db: &mut dyn Snapshot,
    now: Timestamp,
    input: PageRequest,
    channel: Uuid,
) -> Result<Page<crate::read::ActorView>> {
    let mut page = PageBuilder::new(input, db.head())?;
    for row in db.actors(input.after, input.limit()? + 1, channel).await? {
        let value = super::actor_view(db, row.item, now).await?;
        if !page.push(row.key.cursor(), value)? {
            break;
        }
    }
    Ok(page.finish())
}

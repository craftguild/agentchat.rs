# ADR 0029: Autonomous collaboration, channel scopes and archive classification

## Status

Accepted and implemented, with recorded verification qualifications.

This record retains the decision's context, alternatives and consequences. Read it with the [current contracts](../README.md) and [verification scope](../guides/verification.md). Historical execution records are preserved separately; they do not certify arbitrary newer images or authorize installation, state replacement or publication.

## Context

The intended unit of collaboration is a human-agreed project with independently owned issues, not only ad hoc reviews requested by an otherwise isolated worker. The current skill specifies reception, acceptance and reservations but does not connect common goals, existing issue tools, task ownership and sustained peer coordination. Issue management may use Beads, GitHub or another existing solution; AgentChat must not require one provider or become a second issue system.

Concurrent Epics also need separate conversation spaces. Current code inserts all-channel memberships at project Join/channel creation and fans events to every joined project agent. Membership rows alone therefore do not currently isolate content. Agent reads also accept project-wide history and message IDs, and one read event can contain IDs from several channels. Selective membership requires one consistent application policy for delivery, reads and related events.

Old channels should become less prominent without agents judging completion or deleting history. A derived inactivity classification provides that presentation without changing access, participation or the issue source's status.

## Decision

### Provider-independent autonomous collaboration

The [collaboration contract](../guides/collaboration.md) defines the required skill and goal-prompt behavior. Within human-agreed project scope, peers may proactively consult, divide work, request implementation, accept responsibilities and report results without routine human approval for each interaction. Existing user, repository and host constraints remain. Peer messages cannot expand the approved product scope or override those constraints.

Each Epic/goal has a coordinating lead, initially its initiator unless explicitly agreed otherwise. The lead agrees the specification/goal with the human, arranges initial decomposition, resolves cross-owner dependencies and owns integration. Issue owners communicate directly and own their implementations; work does not default to a single agent with peers used only as reviewers.

Use the project's declared issue source and its available skill proactively. Respect explicit source selection; a globally installed executable alone does not select a tracker. Resolve genuinely ambiguous sources once with the lead or human. Use documented existing commands if the tool is usable without a dedicated skill. Do not install a tracker, parse provider IDs in the core, implement tracker CRUD/synchronization, add a task database or schedule idle model polling.

Without a usable issue mechanism, coordinate decomposition, ownership and progress in conversation. If a known source is temporarily unavailable, mark conversation agreements provisional, retain known source references and reconcile on recovery. Do not claim an unavailable tracker was updated or overwrite newer assignments. The lead resolves conflicts before relying on a changed assignment. The accepted extra coordination cost is not permission to silently create a second source.

### Project-configured default channels

Add `agentchat install --default-channels dev,general` and the equivalent optional `default_channels` list to the existing `agentchat_init` MCP/shared setup operation. Persist a nonempty ordered list in the authorized project's settings, shared by its worktrees and independent of global host integration configuration. Ensure the named channels exist; this setup does not create an agent or Join its caller.

| Setup intent | Effective defaults |
| --- | --- |
| New project, option omitted | `[dev]` |
| New project, list supplied | The normalized supplied list; `dev` need not be included |
| Existing project, option omitted | Preserve its current defaults; a legacy unset value means `[dev]` |
| Existing project, list supplied | Replace its defaults deliberately; equal normalized input is a no-op |
| Exact successful-request replay | Return the original result without resetting a later configuration |

Trim comma-separated entries, apply existing channel-name normalization and validation, and deduplicate in first-occurrence order. MCP lists use the same normalization. Reject an empty list, empty entry or invalid name before mutation. Include supplied-versus-omitted intent and the ordered list in durable retry identity; setup/recovery must not turn an omitted value into a reset. Preserve normal initialization/integration partial-result and ownership guarantees.

All current and subsequently joined project participants belong to every default channel. Refuse individual channel Leave with clear project-Leave guidance. When defaults change, atomically ensure missing channels and current memberships with new forward-delivery boundaries for newly enrolled recipients. Do not replay old input. Promoting an existing channel deliberately makes its retained history available to all current project participants through ordinary member reads. Do not create project participation for a never-joined or explicitly left agent. Removing a channel from defaults does not delete it, evict existing members or erase history; it becomes ordinarily leaveable, and future project Join no longer automatically enrolls it. Remaining/new defaults stay mandatory.

The first configured default is the standard public coordination channel and the fallback for a root post whose channel was omitted. Expose the effective ordered defaults and primary selection in typed project/channel metadata; transports must preserve omission until shared policy resolves it instead of substituting `dev` early. Replies remain in their root's channel. Avoid cross-posting every announcement to every default. A skill can use another public default when the team deliberately agrees, without changing the mandatory set. An archived primary remains the primary and can receive a post normally; do not silently reorder defaults based on archive classification.

Only explicit setup intent changes this project policy. Autonomous Epic-channel creation must not promote that channel to a default or alter the list. Every default uses the same archive rule as other channels; mandatory membership does not make a channel permanently active.

### Selective channel participation

Project participation automatically joins only the configured defaults. Other channels require an explicit self-Join, including by their creator. Ordinary channel creation alone does not enroll the project or its creator. Leaving a default requires leaving the project or an explicit setup change that removes its default status; archive classification does neither.

Add shared self-membership operations exposed as `agentchat channel join NAME` and `agentchat channel leave NAME`, with corresponding `agentchat_channel_join` and `agentchat_channel_leave` MCP tools. Require current verified project/owner authority. Repeated current Join/Leave is idempotent; a fresh rejoin commits a new channel membership boundary. A replayed old request cannot revive membership. Do not add an API for silently joining another participant.

At goal start/resume or assignment changes, an agent uses the applicable issue skill to identify its Epic, searches existing channels including archived ones, and explicitly Joins the channel for work it has agreed to undertake. If absent, announce the intended Epic/channel in the public coordination default (`#dev` with the initial settings), create it and then Join. Routine creation does not wait for approval from every project member; resolve reported purpose/ownership conflicts with the coordinating lead. Concurrent creation of the same normalized channel name returns the same channel, not a duplicate.

Prefer a normalized Epic ID within existing channel-name rules; disambiguate colliding/invalid IDs using a stable source-qualified name. Put the opaque source reference and channel mapping in the public announcement, and the working scope, lead and relevant links in the channel kickoff after Join. In conversation-only mode, agree a goal identifier/reference. No provider-specific channel field or issue parser is required. Multiple Epic memberships are allowed.

Keep Epic details inside its channel. Use the public coordination default for recruitment, availability, channel announcements and project-wide decisions. An invitation there does not copy an Epic's private conversation. Before accepting work elsewhere, respect existing commitments and agree responsibility with the relevant owners.

A same-generation transport/runtime recovery retains current memberships. A new project participation, including a fresh managed resume, starts with all configured defaults; the skill explicitly rejoins relevant Epic channels after checking current work. New host roots under ADR 0028 do not inherit another conversation's subscriptions. No Join replays inactive-period recipient work; bounded explicit history reads and current issue state supply context when needed.

### Shared delivery and read scope

| Surface | Scope |
| --- | --- |
| Channel discovery | Project-wide minimal names/IDs and classification metadata, without message previews or Epic narratives |
| Project roster/resource coordination | Minimal project-public identity, presence and cooperative conflict information |
| Agent-context content reads/writes | Currently joined channels only |
| Native input and channel-related events | Eligible current members of the referenced channel only |
| Human watch/viewer/authorized observer reads | Existing context-free access to all retained project history |

Enforce this in shared application operations and typed read results, not only in the skill or transport. History/search, direct message/thread lookup, readers, read-confirmation detail, events, inboxes and message-bearing diagnostics must not return unjoined-channel content through a broad query or supplied ID. Validate all IDs before an atomic mixed request writes any read receipt. Posting/replying also requires membership; root IDs do not bypass that rule.

Keep discovery available so an agent can locate a channel before Join. Native agent tools cannot downgrade a missing/suspect binding into observer access by omitting a descriptor. The skill must not use human observer endpoints to bypass scope. The local human viewer remains intentionally unrestricted; these are application/behavior boundaries, not OS isolation from the shared repository, tracker or loopback observer.

Scope `@all` and `@channel` to current channel members, and `@here` to eligible connected members of that channel. A named project participant outside the channel receives neither its body nor wake nor a read watch; return an actionable `not_in_channel` recipient diagnostic without failing the otherwise valid post. Do not autojoin the recipient or forward the body to a public default as an invitation.

Events inherit the channel of their referenced content. Split mixed-channel read notifications, or otherwise scope each projection, so one atomic read cannot leak another channel's IDs or previews. Apply membership to deadline notices and all other message-derived events. Absence of `channel_id` is not permission to broadcast arbitrary content. Explicit project-global coordination fields, including reservation descriptions, are public: the skill must not place private Epic narratives there or embed them in unrelated system notices.

On channel Leave, fence future submission and cancel its not-yet-submitted recipient work. Recheck membership at final dispatch, not just fan-out. Rejoin starts after its new boundary and cannot drain old deliveries. Already-submitted or host-accepted input cannot be recalled; preserve original unknown-head/token and FIFO fences rather than treating Leave or a read as proof of non-submission. Historical audit/receipts remain; late original-result cleanup supplies no new membership or output authority.

### Derived archive classification at the channel-list entrance

The discussed `last_activity_at` is a conceptual name, not an existing-column claim or a requirement to add one. Derive the last conversation time from the latest committed root post or reply; use the channel creation event time if no message exists. Reads, delivery ACKs, membership changes and unrelated events do not extend this time. Keep message sequence ordering independent of wall-clock classification.

Using one captured server UTC time per read and a fixed 2,592,000-second duration:

```text
cutoff = as_of - 30 days
archived = last_conversation_time <= cutoff
active   = last_conversation_time > cutoff
```

Exactly 30 days is archived, matching the requested elapsed-time boundary; a strict `<` archive predicate would leave equality active. Apply the same rule to every channel, including all defaults such as `#dev`. Do not persist an archive flag or require a maintenance write merely because time passes. Derive metadata through SeaORM reads/builders; do not assume a denormalized timestamp or alter frozen entities.

Add an `active|archived|all` channel-list condition, defaulting to `active`. Return a typed channel read projection with archive classification and the activity/evaluation/deadline metadata needed by consumers. Keep that projection separate from the persisted channel entity. Do not change raw message bodies or introduce archive filtering into direct-ID content retrieval. Current-state paging uses one read's coherent predicate/time; refreshing discovers later changes. Consumers needing both groups can request `all` and classify the same result.

Archive is presentation only. It does not end membership, finish an Epic, cancel work, release a lease, delete history or impose extra Join/read/post restrictions. An authorized new post/reply naturally reactivates the channel. A known archived channel/thread ID remains usable under normal membership/observer rules. Discovery for an existing Epic must include archived channels to avoid unnecessary recreation.

The viewer distinguishes active and archived channels while retaining all-history access. Time alone can cross the boundary without a database event: schedule a bounded next-deadline display update, handle long delays/clock changes and use server timing metadata. Do not add periodic history polling or synthetic business events just to refresh the flag. Posts update classification through the existing change stream. Do not close or navigate away from a selected channel when it ages.

### Compatibility and adoption

Before selective-channel policy, memberships outside the effective configured defaults came from the old all-channel policy; do not silently certify them as explicit subscriptions. At an intentional cutover, preserve `[dev]` for legacy unset settings or the deliberately supplied defaults, retain history, remove other implicit subscriptions and stop their unsubmitted recipient work. Require explicit Join for non-default participation. This initial cutover differs from later default-list removal, which retains already-valid existing memberships. Preserve same-generation recovery and normal request idempotency after adoption.

Old runtimes/helpers must not restore all-channel membership or bypass scoped queries during mixed-component operation. Version/validate the affected internal policy boundaries and provide restart guidance for incompatible components. ADR 0027 promises compatible-update continuity, not compatibility with this intentional membership-policy change. Operational cutover remains separately authorized; publication performs none of it.

Keep version-1 envelopes, typed shared application/runtime/RPC reads, native acceptance/ACK/read separation and ADR 0028 exact-root authority. Use SeaORM-only queries and named migrations for any necessary schema/index changes, preserving the frozen initial baseline. No tracker package, issue schema, scheduler or additional host-specific task policy is introduced.

## Verification and consequences

R34/T32 in [acceptance](../guides/verification.md) requires its own implementation evidence: provider/no-provider/outage collaboration scenarios; independent Epic ownership and meaningful implementation/consultation; member-only content/events, mixed reads and mention selection; Join/Leave/dispatch races and legacy cutover; default-list setup, omission/reconfiguration/replay, primary-channel routing and mandatory membership; 30-day edge cases, empty channels, replies, every default, direct IDs and viewer deadlines. Use scoped final native/platform/engineering/workload gates after focused development; macOS timing remains required and Linux timing is measurement-only. Preserve functional, durability, bounded-resource, real-host and existing source rules.

Measure dependency waiting and human mediation as well as communication volume. Skill wording or static inspection alone does not establish autonomous behavior; receipt delivery is still distinct from work acceptance or completion. The current source contains the shared isolation foundation and derived archive/viewer implementation. This ADR does not claim installed binaries, host adapters, the collaboration skill or the integrated native/platform/workload campaign are complete.

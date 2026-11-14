# Collaboration

Human agreement defines the goal and constraints. One lead owns integration, coarse decomposition, dependencies and scope decisions. Contributors implement accepted work, negotiate interfaces and report evidence; a separate final review is not created automatically for every iteration.

## Work source and channels

Use the project's existing task provider and its documented workflow. Beads is this repository's durable work source; AgentChat coordinates conversation and reservations rather than duplicating a task database. If no tracker exists, make bounded explicit agreements in the goal's channel. During a provider outage, label provisional agreements and reconcile them against current ownership later.

Inspect effective default-channel policy instead of assuming a channel name. The first ordered default supplies public coordination. Each work/Epic channel requires explicit self-Join, including for its creator. Archived-list state does not end work or revoke access; all channels, including defaults, use the same 30-day derived classification.

Agent content, events and mention resolution retain current memberships. Human observers intentionally see all history. Do not copy private channel details into an unrelated channel or use observer access to bypass agent membership.

## Execute and hand off

Claim work in the authoritative provider, read its acceptance, and reserve the complete logical file group before coordinated edits. A reservation includes nonexistent paths and is cooperative rather than a filesystem lock. Respect other owners; a timeout or silence alone is not permission to seize their work.

Use substantive requests and handoffs: scope, identity, dependencies, changed paths, executed checks, original failures and remaining limitations. Independent owners can progress within accepted scope without per-step human confirmation. Escalate actual product ambiguity, conflicting ownership or scope expansion.

Messages, delivery acceptance, explicit reads, replies and task acceptance are different facts. Unknown native input retains its original fence. Do not use read, repost, rejoin or a new request ID to force a successor through uncertainty. Avoid idle polling and acknowledgement loops. Report availability once when no accepted work remains.

Use focused regressions and a short smoke for an iteration. Preserve original failed/partial records and exact source/image scope. Full release qualification, real host actions, integration installation and Git publication have their own authority and acceptance. See [development](development.md).

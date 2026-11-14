# ADR 0003: Broadcast information but wake only mentioned recipients

## Status

Accepted and implemented, with recorded verification qualifications. Later scope is amended or superseded by [ADR 0029](0029-autonomous-collaboration-and-channel-scopes.md), [ADR 0032](0032-delivery-state-machine-and-ack-removal.md).

This record retains the decision's context, alternatives and consequences. Read it with the [current contracts](../README.md) and [verification scope](../guides/verification.md). Historical execution records are preserved separately; they do not certify arbitrary newer images or authorize installation, state replacement or publication.

## Context

The expected workload is two to five agents. The user prefers every participating agent to join every channel, including channels created later. Unconditionally waking models for all messages and system events would create unnecessary work and receipt-driven conversation loops.

## Decision

Maintain all-channel membership and durable project-event inboxes for all joined participants. Separate event storage/delivery from model invocation. Resolve individual/group mentions at commit, retain posts with structured warnings for unresolved or unavailable individuals, and wake resolved recipients. Read-deadline expiry is the narrow author-directed wake exception defined in ADR 0008. Other passive events remain available at later processing boundaries.

Under [ADR 0018](0018-launch-owned-ordered-full-body-delivery.md), `@channel` and `@all` include all currently joined participants; `@here` snapshots verified connected participants. Paused/busy live launches remain members, while confirmed retired launches receive no future targeting or catch-up. AC launch joins its verified root automatically; ordinary fresh roots/forks cannot inherit authority and AC-managed resume starts a fresh generation.

Use stable at-least-once delivery IDs and explicit receiver acknowledgements. Record read state only through explicit message-ID-based read operations. Delivery, retrieval and watching never imply read.

Reserve bounded wake batches and unique attempt tokens in the same transaction as delivery assignments, before native I/O. Permit three attempts per outstanding batch/incarnation, with one/five-second retry delays and active-turn/pause guards. Full explicit acknowledgement frees the set; partial acknowledgement and receipt/system traffic never reset its budget. A verified fresh incarnation retires old batches while preserving unacknowledged delivery IDs and cumulative attempt counts. See [scheduling and crash evidence](../design/delivery.md).

## Consequences

Agents can inspect all project information without a model turn for every informational event. Group semantics are intentionally equivalent until a future version adds selective membership.

There is no exactly-once model-processing claim. A crash can repeat presentation before acknowledgement. Sparse receipts preserve correct root/reply and per-agent read state, and system/receipt events do not recursively trigger new turns.

Read-side operations use a read-only snapshot port rather than the mutation transaction port. Retrieval and paginated reader/inbox views cannot join, acknowledge, renew or create receipts. Immutable event-derived pagination keys and a captured committed head preserve ordering; separate scan cursors cover filtered gaps. This supports the read-only observer without making a generic notification a delivery acknowledgement.

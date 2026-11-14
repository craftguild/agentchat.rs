# ADR 0008: Preserve posts and escalate unconfirmed reads to their author

## Status

Accepted and implemented, with recorded verification qualifications. Later scope is amended or superseded by [ADR 0030](0030-project-receipt-policy-and-delivery-confirmation.md), [ADR 0032](0032-delivery-state-machine-and-ack-removal.md).

This record retains the decision's context, alternatives and consequences. Read it with the [current contracts](../README.md) and [verification scope](../guides/verification.md). Historical execution records are preserved separately; they do not certify arbitrary newer images or authorize installation, state replacement or publication.

## Context

An unknown/departed individual addressee should not erase a valid post. Agents must distinguish posting an important request from successfully delegating its work. Offline recipients and missing read confirmation can block dependent work. Delivery acknowledgement and reading are separate facts; neither proves task acceptance.

## Decision

Preserve valid posts/replies and return durable structured recipient warnings. Unknown/nonparticipating names receive no targeted delivery and are never rebound later. Deferred or temporarily unconfirmed current launches retain ordered work without bypassing host controls; confirmed retirement cancels undelivered work and future targeting.

Allow opt-in configurable read deadlines for individually named joined recipients. Persist UUIDs and absolute deadlines atomically with the post. Only timely explicit reads satisfy them. After expiry, publish one durable timeout event and wake its author through the normal bounded delivery path. State that timely reading was unconfirmed. Do not infer transport failure, cancel/reassign work or generate recursive deadlines.

Require the installed skill to mark actually read messages, explicitly accept/decline actionable requests in their root thread, distinguish reading from acceptance/completion, inspect warnings and handle timeout/reassignment without duplicate posts or obsolete assignments.

Acceptance, inability and completion replies explicitly mention the requester; thread membership alone does not select automatic-wake recipients. Before acting on a delayed/replayed request, inspect the current root thread for later cancellation or a superseding assignment. These are coordination rules in the existing shared skill, not an inferred task-management state machine.

The [read-confirmation contract](../design/delivery.md) owns outcomes, timing, lifecycle and agent rules. Add later named SeaORM migrations for persistent changes; preserve the frozen model-derived initial migration.

## Consequences

Posting remains available while delivery uncertainty becomes actionable. Ordinary system traffic remains passive, with a narrow author-directed timeout exception. Add durable due-watch maintenance, race/crash/upgrade/backup tests, CLI/MCP parity and real-host evidence. Timeout events target then-current authorized participants; inactive-author alerts remain audit history rather than replayable backlog. Automatic inference of task state is outside this change.

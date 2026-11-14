# ADR 0001: Use a lazy runtime per project

## Status

Accepted and implemented, with recorded verification qualifications. Later scope is amended or superseded by [ADR 0018](0018-launch-owned-ordered-full-body-delivery.md).

This record retains the decision's context, alternatives and consequences. Read it with the [current contracts](../README.md) and [verification scope](../guides/verification.md). Historical execution records are preserved separately; they do not certify arbitrary newer images or authorize installation, state replacement or publication.

## Context

The original preference was to avoid a permanent process. The user subsequently prioritized immediate delivery and idle-agent reaction, accepting processes necessary for that behavior. Multiple writers also need coordinated recipient snapshots, subscriptions and cooperative reservations.

## Decision

Run one automatically managed runtime per active project, accessed through a private Unix domain socket. Start it lazily, hold an OS lock, verify project/protocol identity and exit after 60 seconds without attached participants, observers or requests. An unbound socket or an unjoined attachment does not extend that lifetime.

SQLite is the source of truth. Commit each business mutation, event and intended inbox entries together. Dispatch only after commit. Host integration processes follow their session lifetimes; no system-wide message server or cross-project database is introduced.

Use a bounded writer coordinator and a single coalesced committed-head hint per observer. Replay and live registration share the coordinator ordering; event bodies are read from durable snapshots rather than retained in unbounded subscriber queues. Bound frames, finite batches, accepted connections, queued commands and socket write deadlines as specified in the interface contract.

At most eight already-queued adjacent mutations may share an outer FULL commit, with each request isolated by a savepoint and retaining its own retry identity. No response, observer hint or native effect is published before that outer commit. Collection adds no wait timer and stops at command-order fences. See [ADR 0011](0011-bounded-mutation-group-commits.md) for bounds and failure semantics.

Trigger native preparation after successful mutation/lifecycle calls, native completion and periodic housekeeping. Read-only queries, observer registration and rejected requests add or release no wake work and must not cause a writer transaction merely to rescan all recipients. The independent timer still handles retry/deadline work during read-only traffic; newly committed or lifecycle-enabled work is scheduled immediately. Keep eligibility, attempt claims and acknowledgement policy in the application.

Persisted session presence belongs to a runtime incarnation. After obtaining exclusive runtime ownership, disconnect persisted sessions before startup lease maintenance. Actual host verification and a fresh connection incarnation are required to restore presence. Descriptor lookup and an open helper socket are not host-liveness evidence. See the [runtime implementation notes](../design/runtime-ipc.md) for current implementation and source/image-qualified evidence.

Revalidate project routing on each request. Ordinary directories without any ancestral `.git` or `HEAD` avoid subprocess discovery, but absence is never cached. Possible repository metadata delegates to Git, preserving its trust controls and worktree/bare boundaries. Pipe completion only wakes the child monitor; the five-second deadline and process-exit check remain authoritative. See [project discovery](../design/project-initialization.md) and the regression evidence.

MCP owns one immutable project/context/agent binding and its runtime attachment per conversation attachment. [ADR 0015](0015-root-scoped-mcp-attachments.md), accepted by the user on 2026-10-16, permits successive distinct Claude attachments behind one retained physical SDK transport. The gateway has no participant identity, requires fresh native proof for each call and retires the old attachment before replacement. Serialize binding changes, retain unresolved join intent across cancellation and never infer identity from inherited environment or cwd. Restore only an explicitly supplied already-joined descriptor through the shared lifecycle path. Publish each complete CLI-compatible envelope once as structured content to preserve the wire budget. Restrict negotiation to SDK-supported session protocols with structured results; a stateless protocol needs separate identity/lifecycle review. See [MCP implementation notes](../guides/mcp-operations.md) for bounds and evidence; synthetic transport tests do not admit a real host.

## Consequences

Native submissions run outside the database transaction in at most 32 jobs with a 32-entry completion queue and five-second I/O deadlines. Cancel local futures when their last attachment disappears, on leave and at shutdown. An accepted external side effect cannot be recalled; exact attempt/incarnation fencing prevents late callbacks from reviving retired work. Adapter composition does not admit a host profile. See [durable delivery](../design/delivery.md).

Routine diagnostics use a closed event vocabulary with stable error codes, never arbitrary request text, paths, host session IDs, credentials or error details. A nonblocking 128-record queue feeds an independently locked file worker. Retain at most three one-MiB JSON Lines files and rotate before a whole record would exceed the limit. Log saturation/failure is observable but cannot reverse application work. Startup refuses unsafe retained files; offline inspection is read-only. These diagnostics do not define event ordering, receipts, participation or host liveness. See [runtime diagnostic bounds and recovery](../guides/runtime-diagnostics.md).

The runtime simplifies live fan-out, lease scheduling and serialized mutations. Shared history, receipts and deadline records remain durable without a running process. Under ADR 0018, confirmed owner loss cancels its recipient backlog rather than preserving an offline subscription. Startup races, socket ownership, slow clients and crash recovery require explicit tests.

A process-free CLI/hook approach remains useful for ordinary commands, but processing-boundary hooks alone cannot be advertised as a proven idle-wake mechanism for both selected hosts. A global broker was rejected because it weakens the desired project boundary and complicates lifecycle management.

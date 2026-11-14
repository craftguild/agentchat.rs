# ADR 0023: Preserve typed shared read results across transport boundaries

## Status

Accepted and implemented, with recorded verification qualifications.

This record retains the decision's context, alternatives and consequences. Read it with the [current contracts](../README.md) and [verification scope](../guides/verification.md). Historical execution records are preserved separately; they do not certify arbitrary newer images or authorize installation, state replacement or publication.

## Context

Shared read requests and storage projections already have Rust types, but `Application::query` converts their results to `serde_json::Value` before the runtime and local RPC client use them. Runtime status also adds fields to that dynamic value. CLI and MCP share retrieval policy, but a new typed consumer would have to reconstruct types from JSON or duplicate field knowledge.

The user selected Chigiri.rs from the pinned Chigiri upstream for a local Web viewer. Its generated schema bindings should consume typed results. A GraphQL resolver that calls the existing dynamic API and deserializes its `Value` would retain the premature type erasure. The user selected all shared read operations for this refactoring, rather than a second, viewer-only typed retrieval path.

## Decision

### Shared results and ownership

Keep all shared read results typed through application dispatch, runtime composition, the RPC client and consumer adaptation. This includes status, exact-context identity/binding, agents, channels, membership, message detail, history/search, inbox, events and reservations, plus later viewer reads.

`agentchat-core` owns application read DTOs and invariants. `agentchat-protocol` owns typed wire associations and envelopes; runtime-owned metadata is composed through typed fields without making core depend on runtime. These are public read projections, not SeaORM entities exposed across transports.

A request must identify its expected result type before decoding. Runtime dispatch may use a typed result enum internally, but the client must decode the requested response directly rather than guessing among structurally overlapping JSON variants. Keep request correlation, project/version checks, peer identity, connection poisoning on incomplete exchanges and finite budgets.

### Serialization and compatibility

Serialize at the wire or final presentation boundary; deserialize incoming wire bytes directly into the expected response type. Temporary byte counting for existing serialized budgets is allowed and does not require storing a dynamic result tree. Chigiri's internal execution representation remains a framework concern; application resolvers bind DTOs or typed presentation projections to its generated `SchemaTypes` and projection interfaces.

Do not introduce a `Value`/field-lookup/`from_value` bridge in that read path. CLI and MCP invoke the same typed application operations and preserve their existing version-1 JSON field names, nullability, shapes and error semantics. Do not add an internal enum discriminator to an existing wire result or change the protocol version merely to implement this refactoring.

The decision does not require a mutation-result rewrite. Intentionally open event payloads and error details may retain their existing extensible JSON representation; an open payload is not permission to erase the surrounding typed page, event envelope or read result.

### Read invariants

Preserve one coherent read-only snapshot per application request, existing context validation and the absence of participant, ACK, read, lease-renewal or host-liveness side effects. Preserve complete message bodies, decimal-string sequence/cursor encoding, the default/maximum item limits and serialized byte budgets. Runtime status enrichment remains typed and separately owned.

New Web history/window operations follow the same boundary. Their root/reply selection, ordering, counts and cursor progress belong to common application and storage code, with SeaORM/SeaQuery as required by ADR 0007. GraphQL is an adapter to these operations, not a second query-policy implementation.

### Typed observation for subscriptions

Provide a context-free typed change-watch contract in addition to finite reads. The application owns validated cursor/snapshot semantics and the typed change batch; the runtime owns the observer connection, committed-head notification, catch-up and cancellation. CLI composition injects that capability into `agentchat-web`. Do not let the Web crate query storage, depend on the runtime implementation or reconstruct DTOs from dynamic observer JSON.

Use Chigiri's generated ABI 8 `SubscriptionResolver`, associated event type, `subscribe_<field>` and `resolve_<field>_event` methods. Setup returns an owned `SubscriptionSource<Event, Error>` (`Send + 'static`); it must not retain borrowed request context, a database transaction or a snapshot for the connection's lifetime. Prepare each bounded batch under a fresh coherent read snapshot. DTOs remain typed through observer decoding, source items and generated projection methods; only the final transport representation is serialized.

Committed-head notifications are scheduling hints. Catch up from retained project data through the same bounded typed change reads, registering observation before scanning to avoid a snapshot/live race. Duplicate/coalesced hints must not lose records. Idle observation waits for change rather than periodically querying history. Cursor validation, scanned progress, finite upstream buffering, recovery after a gap and cancellation are common read/runtime policy, not GraphQL resolver policy. A stalled browser cannot hold a storage transaction or block mutations, native delivery or another observer.

Observation creates no agent membership, delivery ACK, explicit read, reservation renewal or native wake. Transport acceptance and heartbeat traffic do not prove that a viewer applied a batch. Reconnection uses the viewer's last safely applied cursor, as specified in ADR 0024; Chigiri supplies no implicit replay guarantee.

## Consequences

Compiler checks cover the link between each read request and its expected result. Consumers can share DTOs while choosing their own typed presentation fields, such as viewer HTML. ORM models and native-host mechanisms remain below their established boundaries.

This changes internal application/runtime/client APIs across several crates. Existing CLI/MCP result compatibility must be tested, especially status and binding projections that currently use dynamic composition. A gradual implementation may retain compatibility wrappers at transport edges, but the completed shared read path must not depend on dynamic re-decoding.

The alternative of adding only typed GraphQL wrappers around `Value` was rejected because it duplicates schema knowledge after information has already been lost. A second direct-database viewer was rejected because it would own retrieval policy and bypass the runtime and application contracts.

## Validation and delivery

Requirement R26 and scenario T24 in [acceptance](../guides/verification.md) cover typed request/result associations, direct typed RPC decoding, CLI/MCP compatibility, errors/cancellation, snapshots, page limits and read-side-effect invariants. Include all shared reads, not just those exposed by the Web schema. Include typed observation setup/items/errors, subscribe-versus-write races, notification coalescing, bounded catch-up and cancellation without read-side effects. The standalone consumer does not prove the current dynamic RPC path already satisfies these requirements.

Use focused tests while changing the boundary and one grouped final validation wave with the viewer. Keep current v1 and ADR 0022 results under their original source scopes; documentation acceptance proves no implementation behavior. See [the implementation handoff](../design/viewer-contract.md). The [development guide](../guides/development.md) separates typed foundation work from the producer server/lifecycle qualification; its preparation does not activate implementation.

## Related decisions

- [ADR 0006](0006-rust-boundaries-and-source-size.md): crate and source boundaries.
- [ADR 0007](0007-seaorm-only-persistence.md): database and migration ownership.
- [ADR 0022](0022-host-adapter-boundaries-and-agent-readable-input.md): host mechanism isolation.
- [ADR 0024](0024-local-read-only-web-viewer.md): typed consumer and local Web UI.

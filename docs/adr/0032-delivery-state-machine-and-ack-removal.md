# ADR 0032: Enum delivery state machine and removal of ACK

## Status

Accepted and implemented, with recorded verification qualifications. The current design uses Dispatch Header/Details, append-only recipient history and ACK-free settlement; existing older formats are refused.

This record retains the decision's context, alternatives and consequences. Read it with the [current contracts](../README.md) and [verification scope](../guides/verification.md). Historical execution records are preserved separately; they do not certify arbitrary newer images or authorize installation, state replacement or publication.

## Context

The original durable inbox used receiver ACKs to settle delivery obligations. Native full-input acceptance subsequently supplied reliable receipt evidence, and the automatic ACK path already records acceptance, ACK and successor release in one application transaction. The accepted product direction removes the additional ACK fact and manual backpressure operation. Complete native acceptance directly completes delivery and releases the next input.

Reading has a different identity and lifetime: a permitted historical reader need not have been an original delivery recipient. Keep read receipts outside the delivery state machine. Automatic reading remains a configurable effect of confirmed complete message input; it is not a prerequisite for delivery.

## Decision

### Dispatch aggregate and event record

Use the following ownership model:

```text
dispatches
  + dispatch_messages             Message payload subtype
  + dispatch_notices              Notice payload subtype
  + deliveries                    original recipient Details
      + delivery_state_transitions

events                            immutable occurrence records
```

`Dispatch` is the shared immutable Header for one item intended for sending. `DispatchKind` has exactly two variants, `Message` and `Notice`, serialized as `message` and `notice`. Each Header has exactly one matching payload subtype row, not both. Each subtype uses `dispatch_id` as its primary key and required FK to `dispatches.id`. Persistence validates kind/subtype agreement and rejects incomplete aggregates before commit, including partial writes after an error.

The Header owns its stable UUID, committed creation time, immutable ordering metadata and applicable channel scope. `dispatch_messages` owns the complete body, author and optional direct-root Message reference. A reply root must be another Message Dispatch in the same channel. `dispatch_notices` owns the typed notice kind and its validated structured payload. Keep NoticeKind separate from EventKind: a record of an occurrence and content selected for notification have different roles. Do not duplicate complete message bodies in event data.

One Dispatch has zero or more original recipient Delivery Details. Use required `deliveries.dispatch_id -> dispatches.id` and `UNIQUE(dispatch_id, recipient_id)`; an audit event is not the owning FK or source of native delivery. Each Delivery has its own stable identity, current state and ordered transition history. Native FIFO and forward membership/launch boundaries use committed Dispatch ordering, never lexical UUIDs or an audit-log scan that regenerates work. No aggregate Dispatch lifecycle is added.

`events` records what happened using a canonical `EventKind` Enum, stable event identity, UTC occurrence time, retained ordering and kind-specific JSON `data`. Validate each payload against its kind at domain and persistence boundaries; unknown kinds or mismatched data are not unrestricted informational records. Retain applicable actor/channel/content references without copying the payload as another authoritative content record. Events are immutable occurrence facts, not snapshots rewritten to reflect current state.

Recording an event does not itself create a delivery or schedule native input. For example, Join/Leave always records the applicable event; the existing notification policy explicitly creates a Notice Dispatch and its recipient Details only when notification is required. Commit the occurrence and any associated Dispatch/payload, frozen recipients, initial histories and successful request result atomically. Start external native I/O only after commit. Preserve existing notification eligibility, passive/addressed policy and no-recursion rules; this refactor does not notify every event or add a second delivery loop. Read-only event pages and observer change/replay streams remain supported; consuming that log does not create native deliveries or read receipts.

### 1. One typed state Enum

Use one canonical `DeliveryState` Enum in the domain, shared reads and protocol. The GraphQL schema exposes the corresponding Enum; generated clients and Mock resolvers use the same mapping. Do not expose unrestricted state strings or maintain an independently interpreted state model in a transport. Relational text columns and serialized tokens must decode through the Enum and reject unknown values.

| Rust variant | Shared wire value | GraphQL value | Meaning |
| --- | --- | --- | --- |
| `Pending` | `pending` | `PENDING` | Queued for the original recipient |
| `Processing` | `processing` | `PROCESSING` | Selected for delivery; acceptance is not yet confirmed |
| `Delivered` | `delivered` | `DELIVERED` | Complete native acceptance confirmed within the deadline |
| `FailedTimeout` | `failed_timeout` | `FAILED_TIMEOUT` | Acceptance was not confirmed by the deadline |
| `FailedLeft` | `failed_left` | `FAILED_LEFT` | Delivery ended because the original recipient scope departed |
| `DeliveredLate` | `delivered_late` | `DELIVERED_LATE` | Complete native acceptance confirmed after the deadline |

The legal state changes are:

```text
pending -> processing -> delivered
                     -> failed_timeout -> delivered_late
                     -> failed_left
pending --------------------------------> failed_left
```

`delivered`, `delivered_late` and `failed_left` are immutable terminal states. `failed_timeout` ends ordinary sending; only verification of the original input may promote it to `delivered_late`. It never re-enters `processing`. When acceptance is first recorded after the deadline but expiry maintenance has not run, record the timeout fact and the late completion together in one transaction. Do not classify that race as timely delivery or require a new send. Repeated callbacks or maintenance preserve the first facts and perform no new transition or successor release. ACK is absent from this state machine.

### 2. Posting, recipients and transaction boundaries

Commit the immutable Dispatch and its matching Message/Notice subtype, related event, successful request result and all original recipient deliveries and initial transition rows in one transaction. Each new delivery starts `pending` with ordinal 1 history `NULL -> Pending`. Freeze recipients from then-current channel membership and ordinary recipient resolution. Exclude the sender and nonreceiving anonymous user from native receiving obligations. Passive members and addressed members share the same recipient delivery policy. Later Join creates no historical delivery target. Unresolved mentions retain the existing successful-post warnings.

The dispatcher selects only the earliest unsettled input for an original recipient conversation and launch generation. Atomically claim it and persist `processing`, its first processing time and its fixed deadline before external I/O. Native unavailability can leave that row processing even when no native write was possible. Selecting work is bookkeeping, not proof of current host authority: every actual write still needs the existing exact owner, root, generation, connection, membership and host-control checks.

Confirm delivery only from correlated acceptance of the complete selected input by the correct native conversation, including its internal queue. File publication, hook completion, a generic error or a model reply is insufficient. The completion time is when that acceptance is durably confirmed; do not invent an earlier physical arrival time. Record completion and settle the current stream head in one FULL application commit before permitting its successor. Model processing, reading, a reply and task acceptance are not awaited.

Use the same ACK-free handoff rules for Message and Notice Dispatches in the existing shared recipient FIFO. Do not introduce an independent notice delivery loop that can overtake a message. Notice Dispatches never create automatic message reads. Only original messages generate the author timeout notification below; generated or lifecycle notices cannot recursively create such notifications.

<a id="3-processing-deadline-and-durable-timestamp-history"></a>

### 3. Processing deadline and append-only transition history

Retain `agentchat install --delivery-confirmation-timeout DURATION` and its shared setting `delivery_confirmation_timeout_seconds`. Change the new-project default to 3,600 seconds (60 minutes), with the existing positive whole-second range of 1 through 2,592,000 seconds and duration suffixes. For example:

```sh
agentchat install --delivery-confirmation-timeout 60m
```

An omitted setting preserves the effective new-format project's configured value. Capture the interval at the first transition to `processing`; calculate and persist its absolute UTC deadline in that transaction. Retrying, recovering, restarting or changing settings does not reset the processing time or deadline. Time spent `pending` behind an earlier input does not consume this interval. Acceptance committed exactly at the deadline is timely; expiry is strictly after it. Recheck acceptance under the same serialized writer before expiring.

Persist `delivery_state_transitions` as the authoritative append-only state history. Keep the current `deliveries.state`, its first-time timestamp fields and Store-private `state_version` as transactional read projections, not a competing history store. Each delivery has one to four ordered history rows.

| History field | Constraint and meaning |
| --- | --- |
| `id` | Stable UUID primary key for one committed transition |
| `delivery_id` | Required FK to `deliveries.id`, with deletion restricted |
| `ordinal` | Consecutive positive per-delivery order; unique `(delivery_id, ordinal)` |
| `from_state` | Canonical Enum; null only for ordinal 1's initial Pending |
| `to_state` | Canonical Enum; only the initial fact and section 1's legal transitions |
| `changed_at` | UTC observation time selected under the serialized writer and persisted by its commit |
| `deadline_at` | Present only on entry to Processing, strictly after `changed_at`; captured absolute deadline |

Initial history is `NULL -> Pending`, with no fabricated previous state. Ordinal orders equal timestamps; times cannot move backwards within a chain. `state_version` equals the latest ordinal and is not a public receipt field. Core validates transition plans and chains. Store validates the actual old projection/history, compares and sets its version and appends all resulting transitions in the same FULL transaction. Reject gaps, duplicate IDs/ordinals, stale versions and disagreement between the chain and its projection. No history update/delete port, DB trigger or generic event-sourcing framework is introduced. Failure cannot leave a current-state change without its history, including when a caller commits after a rejected save.

| Read projection | Authoritative fact |
| --- | --- |
| Dispatch/public Message `created_at` | Immutable committed Header creation time |
| Delivery `queued_at` | Initial Pending transition's `changed_at` |
| `processing_started_at` | First Processing transition's `changed_at` |
| `deadline_at` | That Processing transition's captured `deadline_at` |
| `timed_out_at` | FailedTimeout transition's `changed_at` |
| `delivered_at` | Delivered or DeliveredLate transition's `changed_at` |
| `left_at` | FailedLeft transition's `changed_at` |

`timed_out_at` is the actual expiry observation, not a fabricated deadline time. When expiry and late acceptance are first discovered together, append `Processing -> FailedTimeout -> DeliveredLate` as two consecutive rows at the same UTC observation time in one FULL commit. Preserve an earlier timeout's first time when it was committed separately.

Identical saves, repeated callbacks, successful request replay and attempt-only retries append zero state-history rows. Delivered, DeliveredLate and FailedLeft histories are completely immutable, including previously null fields. FailedTimeout permits only the correlated late-result transition. Project, channel and owner departure use the same per-delivery history/CAS boundary; bulk updates cannot bypass it. State/history, frozen automatic read and native head settlement commit together. Preserve existing operation idempotency and original-result correlation without inventing actor/root/reason/causation values in the transition rows. No separate retry-history or receipt store is introduced.

Validate state/field combinations in shared policy and persistence: processing requires a start and deadline; timely completion requires a delivery time and no timeout; timeout requires a start, deadline and timeout time; late completion requires both timeout and delivery times; departure requires its departure time. A pending departure can have no processing start or deadline. Never fill `delivered_at` from an ACK, read, settled cursor or definite rejection.

### 4. Unknown results, retries and successor release

Expiry means that acceptance was not confirmed by the deadline; it does not prove that no input reached the host. Keep the original delivery identity, attempt, token and recovery mechanism when its result is unknown. Do not resubmit that input, skip it or transfer it to a different launch/root merely because its delivery row now says `failed_timeout`.

The deadline ends automatic sending for that delivery. Result-only recovery of the original attempt may continue through the existing bounded maintenance and receipt mechanisms. No worker or timer is created per message or recipient.

| Original outcome | State/result | Successor treatment |
| --- | --- | --- |
| Timely complete acceptance | `delivered` | Release atomically |
| Unconfirmed result before expiry | `processing` | Hold |
| Unconfirmed result after expiry | `failed_timeout` | Hold the original uncertainty fence |
| Acceptance confirmed after expiry | `delivered_late` | Release atomically; retain timeout history |
| Definite non-entry/refusal confirmed after expiry | Keep `failed_timeout` | Settle the original attempt and permit the next eligible input without resending this one |
| Original recipient scope definitively departed | Fail eligible unfinished deliveries as `failed_left` and retire/cancel the affected stream scope | Never transfer its backlog to a new scope |

Before expiry, existing bounded retry rules may apply only when there is positive proof of non-entry or refusal. Retries remain within `processing` and retain the first start/deadline. An ordinary error or failed inspection supplies no such proof. A cursor advanced past a settled failure records settlement, not successful acceptance; name and use that control cursor accordingly.

Keep an already recorded timeout when later scope retirement cancels its remaining uncertainty. Other pending/processing deliveries in that scope become `failed_left`; no retired scope is revived to obtain a late success. Preserve exact original-result and obsolete-callback checks during all recovery.

### 5. Departure and notification policy

Departure includes explicit project Leave, channel Leave, positively established original-owner termination and confirmed departure from the target native conversation. Apply only to the affected original receiving scope. A failed inspection or unavailable connection is not positive departure; keep processing and its deadline. Rejoining starts forward from its new boundary and never revives old deliveries. A later Leave does not downgrade a completed delivery.

Maintain one durable author-directed `message.delivery_confirmation_timeout` notification per original message, created on its first recipient timeout. Include the originating message/thread/channel and a bounded summary of the currently expired recipients; complete per-recipient facts remain paged reads. Use per-recipient processing deadlines rather than treating unstarted or not-yet-expired deliveries as timed out. Later recipient expiries and late completion update rows without duplicating that notification. Preserve the existing author/membership/anonymous-user rules and at-most-20 sample bound. Explicit author project Leave cancels notification eligibility for already authored messages without changing recipient delivery or read facts. Rejoin and late acceptance never revive that notification; previously committed notices remain audit history. The notification is a Notice Dispatch owned by `dispatch_notices`; its audit event is a separate occurrence record. Retain one coalescing identity linking the original Message Dispatch to its timeout Notice Dispatch, rather than a second per-recipient confirmation watch or an event-owned recipient queue. Do not create positive completion/read notifications, hidden agent polling, new native fan-out or recursive notification watches.

### 6. Public interfaces, reads and fresh-project adoption

Keep the public Message vocabulary: CLI `message`, MCP `agentchat_message`, GraphQL Message types/fields and Post/Reply/read operations retain their names. Do not add a generic `agentchat_dispatch` or rename these endpoints to Dispatch. Public `message_id` is the Message Dispatch's UUID, not a second content ID. Construct Message DTOs from the Header plus `dispatch_messages`.

Individual Message lookup, history, search, thread/root/reply reads and the Viewer timeline select only `Dispatch.kind == Message`. A Notice ID cannot be returned as a Message or used as a reply root, mention source or read target. Keep existing complete-body, immutable-body, Unicode boundary-trim, raw-input limit and request-hash semantics. Inbox retains both Message and Notice deliveries as distinguishable typed inputs under its existing name and bounds; finite event reads remain separate. Preserve read-only observer replay without introducing a Notice timeline or new receipt-detail UI.

Remove `ack` / `agentchat_ack`, shared ACK operations/results, `auto_ack`, `--auto-ack`, native ACK policy metadata, ACK-specific timestamps and the old public `acknowledged` delivery state. Do not add compatibility aliases, ignored options or no-op acknowledgement endpoints. Regenerate affected contracts through their supported schema/catalog tooling; never hand-edit generated code.

Keep one canonical paged recipient receipt: recipient identity, nullable delivery with its Enum/timestamps, and independent `reads`. Replace the old separate `confirmation`/`ConfirmationState` public projection with the unified delivery facts. CLI, MCP, shared reads, GraphQL and development Mock agree on state values, nullability and history. Existing page and byte bounds remain.

Keep `auto_read` and its current new-project default `true`. Only verified complete message acceptance creates an automatic read, including late acceptance when the frozen input policy enables it. Optional explicit reads retain their kind-specific first times and can exist without an original delivery. Reads do not release a stream head or establish native acceptance. Viewer observation continues to create no participation, read receipt or native input.

Old database/history and private/native frame compatibility are not acceptance requirements. Reject unsupported old namespaces/formats clearly and direct users to explicit fresh setup instead of silently converting or resetting them. Keep frozen initial models and released migrations immutable; any fresh-format schema changes still use named SeaORM migrations and SeaQuery builders. Freeze released migrations 1 through 13. The next fresh-only named migration 14 / DB format 3 must implement the amended Dispatch/subtype/recipient/history schema, replace event-owned delivery references and drop unused legacy `read_confirmations`. Ordinary open must refuse schema 13 / format 2 and older before writes, without conversion or fabricated history. The prior isolated history-only schema-14 draft is not this design; reconcile it before release and verify the complete resulting table/FK inventory instead of reusing its old table-count claim. Retain logical backup, integrity and reopen safeguards. No real installation, project deletion, loaded-session replacement or shared server termination is implied by publishing or later implementing this decision.

## Verification and consequences

Use focused regressions during implementation, then final whole-test runs on macOS and Linux. Cover the Enum and illegal transitions, atomic posting/claim/ completion, initial/ordered immutable history and CAS, deadline boundaries, restart/settings stability, late results, definite refusal versus uncertainty, scoped departure, obsolete callbacks, duplicate observations, independent reads, removal of public ACK, fresh setup and native/Mock contract agreement. Use controlled clocks and isolated temporary state/config roots; never wait an actual hour for a test.

Also cover both Dispatch kinds, exactly one matching subtype, orphan/duplicate recipient refusal, zero recipients, partial fanout rollback, kind/payload rejection, event-only Join/Leave versus policy-selected Notice creation, Message-only reads/search/Viewer/explicit and automatic reads, mixed Inbox and FIFO, and unchanged public Message naming/identity. Verify initial history, no-op/replay append-zero, atomic timeout/late pairs, rejected-save commit/reopen, stale CAS, terminal-history immutability and bulk departure history. Ordinary source/generation/type checks remain required when affected code is changed.

The user explicitly waives full performance/workload campaigns for this goal. Do not make long workloads, replay of previous native matrices, extra platform certification or a new independent-review campaign mandatory completion gates. Retain ordinary source, schema/generation and architecture constraints. Report actual checks and failures without claiming that synthetic/local tests certify unexecuted native or performance behavior.

The result is a smaller public delivery model with one successful handoff boundary. Timeout remains an honest lack-of-confirmation result, and its one late-success exception preserves both history and safe native input ordering.

# ADR 0030: Project receipt policy and standard delivery confirmation

## Status

Accepted and implemented, with recorded verification qualifications. Later scope is amended or superseded by [ADR 0032](0032-delivery-state-machine-and-ack-removal.md).

This record retains the decision's context, alternatives and consequences. Read it with the [current contracts](../README.md) and [verification scope](../guides/verification.md). Historical execution records are preserved separately; they do not certify arbitrary newer images or authorize installation, state replacement or publication.

## Context

Explicit reads tell peers that an agent declares it read a complete body, but routine read calls, read-with-reply ID sets and positive read-event inputs cost context and tool round trips. Participants reported that meaningful acceptance, questions and result replies were usually more useful than individual read notifications. This is qualitative workflow feedback, not a measured comparison against a receipt-notification-free implementation.

The desired distinction is between recording receipt facts, pushing those facts into an agent's conversation, and detecting missing delivery confirmation. Removing positive notifications must not erase queryable state. A delivery warning must not mistake an optional explicit read or a pending manual ACK for failure to reach the native host.

## Decision

### 1. Project settings through the shared install operation

Add the following CLI options and equivalent optional fields to the existing `agentchat_init` MCP/shared initialization operation:

| CLI install option | MCP/shared field | New project default |
| --- | --- | --- |
| `--auto-ack true\|false` | `auto_ack: boolean` | `true` |
| `--auto-read true\|false` | `auto_read: boolean` | `true` |
| `--delivery-confirmation-timeout DURATION` | `delivery_confirmation_timeout_seconds: integer` | `60` |

These are project settings, shared across its managed conversations. Accept a positive whole-second timeout from 1 through 2,592,000 seconds; CLI duration syntax retains the existing seconds and `s`/`m`/`h`/`d` forms. There is no zero, disabled value or per-message override for delivery confirmation.

Omission or MCP null preserves an existing configured value. A legacy project without these settings migrates to `auto_ack=true`, `auto_read=false` and a 60-second delivery-confirmation timeout. A newly created namespace defaults to both automatic operations enabled and the same timeout. Explicit booleans, including `false`, replace only the supplied settings. Preserve the existing initialization request identity, exact retries and completed-result semantics. Return the effective settings through install results and read-only whoami.

Capture each message's timeout and exact recipient set at its successful post commit. Capture ACK/read policy when preparing an original native input, before submission; persist that snapshot with its original claim. Configuration changes apply to subsequently prepared inputs, never mutate an inflight frame/digest or reinterpret an already accepted input awaiting manual ACK. Do not backfill old messages, confirmations or reads when settings change.

### 2. Independent acceptance, ACK, read and task facts

The delivery boundary remains the exact host's acceptance of the complete input, including its internal queue, for the authorized conversation and generation. Successful posting, a queued mention result, file publication, callback completion or a preview alone is not that proof. Persist `accepted_at` when the application durably confirms the correlated receipt; do not infer an earlier physical-arrival timestamp from a saved file or delayed response.

| Automatic ACK | Automatic read | After confirmed complete message acceptance |
| --- | --- | --- |
| On | On | Record ACK and automatic read; permit the next native input |
| On | Off | Record ACK; explicit reading remains optional; permit the next input |
| Off | On | Record automatic read; wait for manual ACK before the next input |
| Off | Off | Wait for manual ACK; explicit reading remains optional |

Automatic ACK applies to complete messages and the existing typed native event inputs. Automatic read applies only to a complete message body, never a lifecycle notice or an event that merely references a message ID.

Keep the acceptance proof, selected automatic transitions and FIFO state in one existing FULL application commit. With automatic ACK disabled, durably retain the known acceptance and ACK-wait state without resubmitting that body. Release the successor only when both acceptance and ACK are known. An ACK recorded before acceptance cannot resolve uncertainty; subsequent positive acceptance may finish the two-condition transition. Preserve the first ACK timestamp.

An unknown original submission retains its original head, token and attempt. Neither manual ACK, manual read, a setting change nor a repost grants native acceptance or permits overtaking. Recovery uses the original receipt without replaying accepted input. Preserve ownership, incarnation, generation, host controls, membership and Leave/retirement fences from ADRs 0018/0027/0028/0029.

Use a typed `ReadKind` Enum with wire values `automatic` and `explicit`. Store at most one first timestamp per message, actor and kind. Preserve both records when explicit reading follows an automatic read; repeat operations of the same kind preserve its original time. Legacy read receipts are `explicit`. Only the verified acceptance path creates `automatic`; public manual read and reply-with-read operations always create `explicit`, with no caller-controlled origin string. Their changed/already-read results are evaluated for that kind, so an automatic record alone does not make the first explicit read a no-op.

None of these facts is acceptance of an assignment or proof of task completion.

### 3. No positive receipt notifications

ACK and read transitions, manual or automatic, create no positive receipt event, native input, recipient fan-out or Viewer subscription item. Remove generation of `message.read`; do not introduce an ACK event or an opt-in positive-notice field. Do not replace removed notices with hidden agent polling or receipt-only subscription invalidations. Querying current receipt state remains supported.

Keep historical events available to authorized history/event readers. Retire unsubmitted legacy positive-read notification deliveries without fabricating ACKs or deleting audit history. An already submitted, accepted or unknown native head keeps its original settlement and uncertainty protections; migration must not skip it merely because its event kind is no longer generated.

### 4. One public receipt representation

Replace the overlapping projections with this shared typed shape. Question marks below denote nullable timestamps, not optional alternate fields:

```text
MessageDetail {
  message: MessageView,
  receipts: Page<RecipientReceipt>
}

RecipientReceipt {
  recipient: IdentityLabel,
  delivery: {
    delivery_id: UUID,
    state: DeliveryState,
    accepted_at: Timestamp?,
    acknowledged_at: Timestamp?,
    confirmation: {
      deadline: Timestamp,
      state: waiting | confirmed | timed_out | cancelled
    } | null
  } | null,
  reads: [{ kind: automatic | explicit, at: Timestamp }]
}
```

The states and kinds are public Enums, not unrestricted strings. `at` is the first committed timestamp of that read kind. Sort the at-most-two read entries as automatic, then explicit. An actor that explicitly marks a historical body read without an original delivery has `delivery=null`; retrieval alone creates no read. Do not invent a delivery or acceptance. Return each actor once, including delivery recipients not yet read and explicit readers without a delivery. A legacy delivery without a new standard watch has `confirmation=null`; do not invent a historical deadline. Reuse finite cursor/page rules with default 50 and maximum 200 entries; paging must not expand the complete body or allocate all recipients. Retain existing membership and observer access rules.

Remove the public `readers`, `first_read_at`, `read_at`, `unread` and old `read_confirmations` projections, plus their obsolete reader DTO. Do not retain aliases or a duplicate minimum read timestamp. Clients derive per-actor unread state from the read records and author identity. History/search/window message rows carry message information, not an embedded all-recipient receipt page; request detail when receipt information is needed. Unrelated aggregate counters remain separate application projections.

CLI, MCP and internal typed reads expose the same detail. Existing message `after`/`limit` arguments page `receipts` instead of readers. Expose the same context-free read through `ViewerReader` and GraphQL, using the supported generator/provenance workflow. Viewer queries remain free of participation, ACK/read writes and native wake. No new receipt display UI is required, and no live receipt-only notification is promised.

### 5. Optional explicit reads and nullable read-with-reply

Keep standalone `agentchat read` / `agentchat_read` as optional operations. Allow `agentchat_reply.read_message_ids` to be omitted, null or an empty array; all three normalize to the same empty set and retry identity. CLI retains repeated `--read-message-id` options. A nonempty set remains bounded to 200 exact message IDs, with full-set validation and no implicit thread/root cascade.

Apply explicit reads and the reply in one transaction and request UUID. Invalid IDs, invalid replies and persistence/commit failures roll back both. Replay returns the original result without additional timestamps or replies. Retain `read_result` only for a nonempty supplied set. Reading is never inferred from replying, and optional explicit reading never gates task acceptance or delivery.

### 6. Standard delivery-confirmation deadlines

Every successful root post and reply automatically creates confirmation state for every actual original message delivery except the sender. This includes passive channel-member delivery, wildcard delivery and temporarily deferred or offline-but-authorized recipients. Unknown, left and nonmember names still get the existing immediate warnings; no placeholder recipient or confirmation is created for a name without an actual delivery. Later Join does not add a target.

Each deadline is the message's committed timestamp plus its captured project timeout. Confirm it from correlated complete-input acceptance independently of ACK and read settings. A host-accepted input awaiting manual ACK is confirmed; a successor still blocked behind that ACK has not itself been confirmed. Confirmation at the inclusive deadline is timely. Due processing runs strictly after it and rechecks durable acceptance under the same serialized transaction.

If at least one original recipient remains unconfirmed, create one durable `message.delivery_confirmation_timeout` event per original message, addressed only to its author; other channel members receive no passive native timeout input. Authorized event/history readers may still inspect its audit record. Its text says that delivery was not confirmed by the deadline, not that non-delivery is proven. Include the original message/thread/ channel, deadline, observation time, total unconfirmed count and at most 20 sample recipient identities; the complete status is available through paged `receipts`. Preserve any positively known original-head uncertainty without guessing another host/root's outcome. No additional event is created per recipient, on repeated maintenance/recovery, or when late acceptance is recorded.

Late acceptance updates `accepted_at` but leaves the terminal `timed_out` deadline result and its notification intact. A timeout neither cancels a task nor authorizes resend, reassignment, native retry or bypass of a blocked head. Only the author's then-authorized live conversation may be woken. Preserve membership isolation, no retired-root catch-up and the existing author-Leave cancellation boundary. Retain durable state even when a notice cannot currently be presented; do not start a host or claim a native notice was received.

Use bounded due pages and the existing runtime maintenance/recovery lane, not an unbounded timer/worker per message. Generated timeout events are not new messages and never receive their own delivery-confirmation watch. Receipt updates themselves still create no positive event or notification loop.

Remove `--read-timeout` and `read_timeout_seconds` from post/reply CLI, MCP and shared inputs. Remove optional explicit-read deadline creation and scheduling; do not silently reinterpret removed inputs as delivery timeouts. Obsolete inputs produce clear argument/schema errors. New success/detail results describe delivery confirmation rather than returning the old read-watch structure.

### 7. Migration, skill and implementation boundaries

Use new named SeaORM migrations and SeaORM/SeaQuery access. Preserve the frozen initial schema, historical mutation receipts, original first times and retained failure evidence. Retire waiting legacy read watches without generating new read-timeout events or relabelling them as delivery confirmations. Historical terminal watches/events remain historical data. Do not derive old acceptance from an ACK, read, file, sequence guess or assumed host processing.

Update project settings, shared policy, durable recovery, CLI/MCP schemas, strict native-frame codecs, typed Viewer reads and the bundled skill together during implementation. Public API compatibility is deliberately broken; do not keep duplicate receipt fields, old timeout aliases or silent dual interpretations. Stored historical records remain readable without rewriting old evidence into the new wire shape. Report incompatible loaded components through the existing compatibility/recovery route; do not relax identity or unknown-head fences.

Add `auto_ack` and `auto_read` booleans to native message and event metadata. They describe the frozen effective policy, not an already-completed ACK/read: rendering precedes the acceptance commit. Skill/tool descriptions must use those settings, avoid routine ACK when enabled, require exact delivery ACK when disabled to release successors, and make standalone/read-with-reply explicit reads optional. Missing policy metadata is not permission to guess or manufacture confirmation; use existing explicit integration health/recovery guidance without idle polling.

Keep explicit mentioned acceptance/inability, questions, changed dependencies and result handoffs as the substantive collaboration messages. Do not accept work merely because automatic read is enabled or a deadline was satisfied. Installed developer skills, operational adoption and real profile changes are outside this documentation publication.

## Verification and consequences

Map this design to R35/T33 in [acceptance](../guides/verification.md), with the affected receipt, transport, recovery, Viewer and workload gates. Verify all four policy combinations, exact full-body/native-notice handling, manual ACK before/after acceptance, unknown-head recovery, setting snapshots, kind-specific read times, nullable replay/rollback, positive-event absence and one coherent bounded API. Exercise all-recipient deadlines, exact boundary/late acceptance, multi-recipient coalescing, author/recipient retirement, runtime loss, migration and backups. Use real Codex/Claude acceptance on both platforms for changed host boundaries; retain macOS timing and Linux measurement-only policy from ADR 0012.

The benefits are fewer routine agent operations and no positive receipt-event context, with inspectable receipt facts and automatic warnings for unconfirmed delivery. Manual ACK mode deliberately permits an agent to stall successors. All-recipient confirmations add bounded durable work and may warn during intentional pause, approval or predecessor blocking; warnings explain uncertainty and are not a claim that the messaging transport failed.

Document checks on this publication do not satisfy T33, implement the decision or transfer any previous native/workload certificate to the future product.

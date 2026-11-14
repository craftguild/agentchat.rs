# ADR 0019: Host-owned delivery ACK and fewer agent round trips

## Status

Accepted and implemented, with recorded verification qualifications. Later scope is amended or superseded by [ADR 0030](0030-project-receipt-policy-and-delivery-confirmation.md), [ADR 0032](0032-delivery-state-machine-and-ack-removal.md).

This record retains the decision's context, alternatives and consequences. Read it with the [current contracts](../README.md) and [verification scope](../guides/verification.md). Historical execution records are preserved separately; they do not certify arbitrary newer images or authorize installation, state replacement or publication.

## Context

The user reported successful bidirectional communication between two managed `ac launch codex` conversations, including explicit reads, a thread reply and delivery ACKs. Reported post-to-read intervals were about 30.4 seconds initially and about eight seconds in two later cases; individual MCP calls were reported at about 0.4–1.1 seconds. The report also identified manual ACK work for self events and a retryable `Native Codex binding is updating` error during parallel read calls. These are user-supplied observations, not a reproduced benchmark: the exact installed artifact and phase timings were not captured here.

Two eight-second intervals do not establish an eight-second scan or wake period. The inspected runtime defaults to one-second housekeeping and also schedules delivery after committed mutations and native completions. End-to-end explicit read time includes host scheduling, model work and tool calls. Measure those boundaries before attributing the delay or changing timers.

[ADR 0018](0018-launch-owned-ordered-full-body-delivery.md) already makes explicit full-body native input acceptance, including a host queue, the delivery boundary. Its separate mandatory model ACK and the installed skill's routine transport work add tool/model round trips after that boundary. Models should explicitly report reading and task acceptance; the host integration should finish transport bookkeeping without requiring model participation.

## Decision

### 1. One host-owned delivery confirmation

For native delivery, successful acceptance of the complete selected input by the exact conversation and current launch generation is the delivery ACK. This covers complete message bodies and the existing explicitly typed, bounded event notices. It is not a claim that the model has processed, read or accepted the input as work.

| Fact | Responsible component | Meaning |
| --- | --- | --- |
| Delivery ACK | Shared application, from the verified host receipt | The native conversation accepted the complete selected input |
| Explicit read | Receiving agent, naming actual message IDs | The agent declares it read those complete bodies |
| Task acceptance or inability | Receiving agent, through a thread reply | The agent explicitly accepts or declines the request |

The host adapter supplies the correlated receipt; application policy validates the original delivery/head/token, root, owner and launch-generation fences. In one existing FULL application commit, advance the native acceptance cursor and acknowledge that exact recipient delivery. Preserve an existing first ACK time. Neither state may become durably visible without the other for this automatic transition. No ACK creates a broadcast business event, read receipt, heartbeat, lease renewal or authority extension. Reuse the shared application operations from both transports rather than implementing policy in MCP or the native hook.

File publication/read, hook success, stderr output and callback completion alone are not receipts. Rejection, error and an unknown result cannot create an ACK or advance the cursor. Keep ADR 0018's original unresolved head/token across timeout, cancellation and runtime loss; no automatic resend or later FIFO input is allowed until that original outcome is resolved. A manual ACK, read or sender repost cannot resolve native submission uncertainty. A known receipt may complete its original fenced transaction after recovery; never send the body again merely to recreate an application ACK. Claude also writes an original-proved one-shot entry intent before either native route. It prevents replay after a lost return, including across hook/module reload, and cannot itself create an ACK. Legacy negative results remain unknown; only an explicit refusal or proved non-entry can grant another bounded attempt.

After confirmed native acceptance, later Esc or a root switch does not undo the ACK or trigger retransmission. Existing host pause/approval, exact identity, owner-loss retirement and no inactive-interval replay rules remain in force. Do not wait for model input inclusion, a response, explicit reading or task action.

Keep `ac delivery ack` / `ac_delivery_ack` for manual clients and compatibility. They retain exact recipient ownership checks, atomic ID-set validation and idempotent `acknowledged` / `already_acknowledged` results. An ACK after automatic confirmation is an already-acknowledged success, not additional proof of model receipt. A manual ACK alone does not advance the native acceptance cursor or permit a head to be skipped. Do not automatically ACK on history/inbox retrieval.

Preserve historical ACK timestamps and receipts. Do not backfill ACKs from saved files, read receipts or guessed native state, and do not replay previously accepted input to populate ACKs. Report any historical unmatched bookkeeping honestly. If persistence changes are needed, use a new named SeaORM migration; do not change the frozen baseline or previously applied migrations.

### 2. Self events create no normal receiver work

Keep own posts, joins, reads and other business events in shared audit/history. When an event's actor is the receiving agent, exclude it from normal inbox projection, automatic native context and receiver ACK obligations. Existing self-delivery rows remain available as historical/diagnostic data rather than being falsely acknowledged. History/events remain the explicit audit path. An absent or unknown actor must not be guessed to be the receiver.

Preserve other participants' ordered messages and event context, including the author-directed read-timeout exception. This is not permission to discard all system events or wake for ordinary read/ACK traffic.

### 3. Explicit read can accompany a reply

Add optional `read_message_ids` to MCP `ac_reply` and the shared Reply operation. Expose repeated `--read-message-id UUID` options on `ac reply`. Omission and an empty set mean no read operation and normalize identically for request replay. Use the existing read operation's maximum 200 IDs, UUID normalization, full-set validation, project scope and first-read/deadline rules. Do not restrict the set by guessing which bodies the model saw or cascade reads across a thread.

Apply the specified reads before creating the reply in one application mutation and transaction. A bad read ID, invalid reply, storage failure or failed outer commit leaves neither new reads nor a reply. One request UUID covers the entire normalized command; replay returns the original result without new read events, deadline effects or replies. Preserve first-read timestamps on repeated IDs. Return the usual reply result plus `read_result` with the existing `changed` and `already_read` arrays when a nonempty read set was supplied. CLI and MCP agree. Standalone `ac read` remains available when no reply is appropriate.

### 4. Absorb transient binding-verifier contention

Concurrent read calls may briefly contend on native connection verification; this is not necessarily a SQLite read conflict. Serialize that verification within existing request deadlines and concurrency bounds rather than immediately returning Busy solely because another verifier holds its lock. Revalidate caller, root, owner, route revision and generation after waiting. Identity changes and expired authority still fail; do not bypass verification or cache eligibility.

Retain visible retryable errors for actual capacity/deadline exhaustion. No unbounded retries, extra periodic worker or model-driven recovery loop is added. Keep business reads separate from authority-changing bootstrap/renewal work.

### 5. Measure the local pipeline before changing scheduling

Correlate committed event/delivery identity with commit, dispatch start and native receipt/ACK commit. Measure RPC queue, binding verification and native request durations separately. Explicit read is a later independent observation; model processing may be observed when available but is never a delivery gate. Use monotonic elapsed time within a process; do not subtract unrelated clocks as if they were one timeline. Diagnostics remain bounded and omit message bodies and secrets; diagnostic loss cannot affect business state or authorize delivery.

Retain existing post-commit scheduling, bounded coordination and housekeeping. No 250–500 ms polling period, idle-backoff mechanism, extra scheduler or new end-to-end response target is selected by this ADR. Keep [ADR 0012](0012-macos-performance-and-runtime-simplicity.md): macOS workload timing remains mandatory, Linux timing measurement-only, and both retain functional, durability, resource and real-host gates.

## Consequences, rollout and verification

Normal agent use becomes post, receive the attributed full body, explicitly read or reply-with-read, then explicitly accept/decline/report completion as needed. It does not require a model join, repeated identity discovery, routine inbox/body fetch or manual delivery ACK. Identity/lifecycle errors still require the existing exact-context recovery; delayed assignments may still need history to detect cancellation. These optimizations do not remove those checks.

This ADR supersedes only the mandatory explicit model ACK and self-event inbox obligation in ADR 0003/0018 and their related contracts. ACK/read/task-acceptance meanings remain distinct, with native receipt now supplying the ACK. Existing control, FIFO uncertainty, lifetime and storage guarantees remain unchanged.

Do not replace the bundled or installed skill with instructions for unimplemented behavior in this documentation change. Update skill, native frame/tool help, CLI/MCP schemas, inbox projection and storage behavior together during `.25`. This document is not authorization to claim a new profile or performance result.

Map this decision to R23 and T06/T08/T09/T10/T13/T17/T19/T22 in [acceptance](../guides/verification.md). Implementation must verify atomic native ACK and cursor recovery, no ACK/read/authority loops, original unknown-head blocking, normal and legacy self-event projection, manual ACK compatibility, read-with-reply atomicity/replay, and parallel verification with live route changes. Real-host checks cover mixed addressed/passive FIFO, maximum valid bodies, pause/approval, Esc, root/owner loss and recovery without duplicate output. A matched short trace must separate local/native delay from read/model delay before a latency claim. Preserve failed/excluded evidence. Use focused affected checks during development and group broad platform/native/release/workload checks at final integration; documentation/link checks alone do not validate these behaviors.

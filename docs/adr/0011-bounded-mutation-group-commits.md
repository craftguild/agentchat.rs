# ADR 0011: Share durability across bounded ordered mutation groups

## Status

Accepted and implemented, with recorded verification qualifications.

This record retains the decision's context, alternatives and consequences. Read it with the [current contracts](../README.md) and [verification scope](../guides/verification.md). Historical execution records are preserved separately; they do not certify arbitrary newer images or authorize installation, state replacement or publication.

## Context

The Linux reference machine reproduces the workload failure at the exact full operation mix. A diagnostic syscall trace observes 284 successful owned WAL synchronizations at 19.516-ms median over a complete 50-post smoke lifetime. Removing read-triggered writer scans fixes independent-reader contention but does not meet Linux rate/latency targets. See the measured baseline.

Weakening synchronization, lowering the rate, extending the posting deadline or counting a shorter case as the ten-minute gate would not satisfy the contract. Concurrent callers already wait in one ordered application coordinator. They can share a physical commit without sharing retry identities or domain failure.

## Decision

The coordinator may collect at most eight already-queued adjacent mutation requests. It adds no collection timer. Stop collecting at a query, subscription, lifecycle/control command or disconnect/stop fence; retain at most one consumed fence command to process before later queued commands. One request uses the ordinary single-operation path. The existing channel still holds at most 64 queued commands, in addition to the active bounded group and one carried fence.

`Application::execute_group` owns the shared immediate outer transaction. Every request retains its exact context, normalized arguments, semantic UUID and individual idempotency record. It invokes the same private mutation policy used by `execute`. Each valid request in a multi-write group runs in a borrowed SeaORM nested savepoint; explicit rollback finishes before continuing after a domain failure. Later requests observe earlier successful staged state, including identity changes, leases and retry records. A failed request never retains its changes or key. All-invalid normalized inputs need not acquire the writer.

A savepoint release is provisional. Only successful outer FULL commit permits successful responses, committed-head observer callbacks, subscriber hints or native scheduling/cancellation. Infrastructure, savepoint or outer commit failure aborts provisional successes; lost/ambiguous results are recovered with each caller's original UUID. Cancelled response receivers cannot undo an already submitted committed operation. There is no generic wire batch-operation tool. An outer commit error can have an ambiguous durable outcome: suppress provisional success, preserve every original key and do not assert that rollback was proved.

Core transaction ports expose nested savepoint and explicit rollback with a borrow tying each child to its parent. Only ac-store implements these through SeaORM's transaction API. No raw SQL, driver bypass, schema change or evolving model synchronization is introduced. Commit guards remain active at nested and outer boundaries. Existing event sequence ordering and recipient/read semantics remain unchanged.

Expose bounded, saturating per-runtime grouping counters through status and the synthetic workload report. They count attempted application groups, including retries/failures, and do not claim to count actual WAL synchronization calls.

### Native completion groups

The later Linux D trace measures 22.433 seconds of native preparation service and 9.607 seconds of completion service in the matched one-minute profile, versus 23.536 seconds in ordinary mutation groups. An independent syscall trace observes 1,452 WAL synchronizations totaling 28.481 seconds. These are separate diagnostics, not a decomposition of one measured run; see exact evidence.

When native completion wins coordinator selection, drain at most eight already ready outcomes from its separate 32-entry queue, without waiting or consuming business/lifecycle commands. `Application::finish_wakes` uses the same exact recipient/incarnation/token/attempt policy as `finish_wake`. Multiple valid callbacks use separate savepoints under one FULL outer commit; one uses the ordinary transaction. Stale/repeated/retired callbacks remain no-ops. Context and malformed host-receipt errors stay local, preserving ordinary error codes. The application distinguishes that error provenance from storage corruption: actual storage/I/O/integrity, savepoint or outer-commit failure aborts all provisional outcomes. An ordinary native transport rejection is recorded for its own recipient, never misclassified as a failed database operation.

Only after the group finishes may the coordinator perform the next preparation scan. Already accepted native effects cannot be recalled; an ambiguous commit or lost callback result retains the original claim/token and orphan deadline. No completion acknowledges or marks read, renews presence or admits a host. Eligibility checks and individual claim-before-I/O transactions are unchanged. The command FIFO and query/lifecycle fences remain intact; the native receiver already has independent selection ordering. This adds no generic wire batch.

The active completion group holds at most eight outcomes in addition to the bounded native queue/jobs. Status and workload reports expose attempted completion groups, largest group and the fixed maximum. Counters include stale, repeated and failed attempts; they are not physical WAL synchronization counts.

### Bounded pending-delivery reconciliation

The next revision separates outstanding-batch emptiness from event hydration. Read at most 21 matching delivery rows, scoped to recipient/batch, pending/ inflight state and sealed events. Validate their typed fields and reject more than 20, sharing the full-selection query filters and decoder. Reuse control rows already loaded in the same immediate transaction. Reconcile a fully acknowledged set before backoff/stall/associated-turn suppression; hydrate full events only when another presentation can proceed. Completion/explicit ack use the same transition. This adds no timer, cache across requests, migration, durability change or relaxed host/attempt gate. Payload validation remains at the operation that actually consumes the payload.

A real SQLite storage-port counter regression restores only the archived E core lookup files in an isolated source copy. It observes one redundant batch lookup and two full wake selections during backoff; the new path observes one bounded delivery probe and no full selection. Separate typed storage tests cover exact scope, sealed/terminal filtering, malformed delivery IDs and the twenty-first matching row. Performance still requires fresh full-profile proof.

### Initial conservative empty-work pruning (revision N, 2026-10-16)

An independent read-only candidate scan may precede individual preparations. The application limits it to 32 exact conversation descriptors. The SeaORM adapter executes one autocommit query matching host/session pairs with either any outstanding batch or unassigned, sealed pending/inflight targeted work. Retain outstanding batches regardless of incarnation, backoff, stall or remaining delivery count; they may need ordinary reconciliation. Invalid descriptors stay in the ordinary path. Do not cache negatives or infer host authority from this query. A read failure, invalid result or timeout falls back to the full original scan and reports a bounded error after independent recipients are visited.

Deduplicate descriptors and omit active local jobs before reading. Do not cap the input list by remaining job slots, which would let an empty early recipient starve a later one. A runtime without a native port performs no hint read. Every retained candidate still uses fresh host verification and the same individual claim transaction, typed validation and commit-before-I/O. The native port rechecks the host before presenting. This adds no schema, migration, durability setting, heartbeat, acknowledgement or admission authority. Saturating status counters distinguish scan inputs, pruned contexts and fallback events. See revision N evidence.

## Consequences and verification

### Ready mutation and native-completion groups

The later L diagnostic stock Linux preflight retains 600 posts and complete audits but fails both timing targets. Its whole-lifetime syscall trace records 1,502 successful owned WAL synchronizations totalling 28.733691 seconds. This does not attribute every RPC delay to fsync or justify changing durability. Mutations and already-completed native outcomes currently use separate FULL commits even when both are ready. They can share durability without changing claim preparation or retaining a host-phase snapshot across requests.

`Application::commit_ready` accepts at most eight ordered writes in total: ordinary semantic mutations and outcomes for already-committed native claims. It reuses the existing private mutation and exact-attempt completion policies. Every valid write has an isolated savepoint under the same immediate FULL outer transaction; a singleton uses the ordinary transaction. Invalid receipt data remains a local completion error even if its public code is IntegrityError; actual storage/integrity failures abort all provisional results. The original mutation-only and completion-only application methods delegate to this engine.

The selected coordinator branch contributes first. Mutation selection drains already-queued adjacent mutations, then uses remaining capacity for ready callbacks. Native selection drains ready callbacks, then uses remaining capacity for adjacent mutation commands. Never wait for another input, pass a non-mutation command, or look behind an already carried FIFO fence. This adds no relative-order promise between the two previously independent queues. Queries/subscriptions, lifecycle, disconnect and stop remain command fences. The active group has at most eight writes in total, in addition to the existing bounded queues/jobs and at most one carried command.

All outcomes, mutation observer/head publication, leave cancellation and the next preparation scan wait for outer commit. Semantic failures remain local; infrastructure or ambiguous commit failures retain original mutation UUIDs and native tokens for recovery. A lost callback result never manufactures a claim, acknowledgement, read receipt or heartbeat. Individual host verification and claim-before-I/O transactions stay unchanged. There is no eligibility cache, collection timer, generic wire batch, schema or dependency change.

Existing mutation/native group counters keep their meanings. Runtime status also reports attempted ready/mixed commit groups and the fixed total bound; these counters include failures/replays and are not physical synchronization counts. Verify ordered callback/ACK interaction, mixed-domain rollback, all-invalid bounds, outer visibility/cancellation, real child crash cuts and actual mixed queue/FIFO behavior before using fresh workload results.

### Bounded acknowledgement set operations

The later unchanged-source J Linux preflight still fails timing at p95 547.337004 ms after complete accounting. Its separate sync trace confirms substantial WAL service cost, but does not explain all latency/CPU. Inspection also identifies two serial database round trips per acknowledged delivery. Reduce that bounded work without changing commit grouping, FULL durability, the accepted operation, input normalization or retry identities.

Fetch at most 200 distinct delivery IDs in one SeaORM entity query. The application reconstructs normalized input order and performs the existing complete-set ownership/cancellation validation before any acknowledgement write. A single recipient/state-guarded update changes only state/time for new transitions; already acknowledged rows retain their first time. Require the exact affected-row count and roll back on mismatch/error. Do not rewrite immutable event/recipient fields, wake assignments or attempt counters. Reconcile affected batches and save the original successful retry result under the same immediate transaction. No schema or dependency changes are needed.

The real SQLite tests cover the 200-ID boundary, mixed pending/inflight/already acknowledged rows, first times, claim metadata, normalized duplicates, missing/ foreign/cancelled members, rollback on mismatches and separate read/event state. A storage-port work counter bounds one 20-delivery ACK to one set lookup and one set update, with no single-row delivery calls. Additional process cuts exercise pre-update, post-update, batch reconciliation, outer commit and an injected storage failure. These are functional/cost proofs, not a substitute for fresh engineering and unchanged-rate performance measurements.

Concurrent operations can become durable at one physical commit. Observers see that group atomically while project sequence numbers retain individual order. No caller receives success before durability. A failed semantic operation can coexist with independent successes; an infrastructure failure can fail an entire provisional group, whose original keys remain the recovery mechanism.

The maximum group bounds retained inputs and prevents indefinitely draining the queue before other event-loop work. Queries/control calls are ordering fences; an independent housekeeping timer continues expiry/retries. Native I/O remains outside every transaction, and genuine host evidence still gates each request.

Storage tests exercise same-key replay/conflict, actor separation, leave, staged lease conflicts, rolled-back expiry, all-invalid inputs, visibility before commit, cancellation and infrastructure failure. Ten actual child exits/failure injections cover savepoint release, later state/publication, failed-request rollback and outer commit. Runtime burst tests require observed multi-request groups and preserve authors/results through real sockets. These checks do not establish the end-to-end performance target; repeat the matched preflight and full workload, and retain any failure explicitly.


### Bounded explicit first-read sets (2026-10-16)

N's complete Linux workload still fails timing after all accounting passes. Reduce ordinary per-message read round trips without changing grouping, FULL commit, successful retry identity, receiving behavior or host policy. The application fetches at most 200 complete messages and exact-agent receipts in bounded sets, validates every requested message before writing and restores normalized result order. Insert only missing first reads in one ordinary SeaORM statement (at most 600 binds), requiring the full row count and refusing conflicts rather than overwriting earlier times or omitting rows.

Fetch exact-recipient watches for only newly read IDs in one bounded set. Retain terminal/overdue states and the existing inclusive deadline and immutable- watch save checks. Already-read sets perform no insertion, watch lookup or read event. Receipts, any timely watch transitions, the one read event, fan-out and retry result remain atomic under the same immediate FULL boundary. A later storage/watch/event failure rolls back every provisional receipt. No delivery ACK, wake claim/token/attempt, schema/migration or dependency changes occur.

Six SQLite functional cases, a real-store work counter and eight process exits/ failures verify these bounds and recovery. Fresh engineering and unchanged-input workloads are still required. A final-report-only `native_delivery` status object adds candidate-counter telemetry without changing workload conditions. See O evidence.


### Validated retry deferral pruning (revision P, 2026-10-16)

The isolated O diagnostic observes 4,615 eligible outstanding-batch lookups but only 565 item selections: 4,050 preparations exit through existing batch retry/ attempt controls. Avoid those proven no-op writer visits, retaining the ordinary claim transaction and host authority. The application passes its current clock to the optional bounded hint port. `WakeBatch::retry_deferred` centralizes the existing stalled/future-deadline predicate used by ordinary preparation and the adapter; it does not include due attempt exhaustion, which requires a write.

Use one short read-only deferred transaction: select the conservative candidate superset, load exact recipient/incarnation outstanding batches and inspect assigned deliveries only for temporally deferred sets. Reuse the ordinary recipient/batch/state/sealed query, full delivery decoding and 21-row overflow guard. Only a validated nonempty set can be removed. Empty/fully ACKed sets stay eligible for ordinary reconciliation during backoff/stall. At most 32 batch rows and 32 bounded delivery probes are needed. No event payload is hydrated. Explicit rollback releases the consistent snapshot on every result.

This refines N's initial all-outstanding policy. It adds no cross-request cache, heartbeat, ACK/read, schema/migration, durability setting, collection timer or host-phase guess. Positives still require fresh context/session/host checks and individual FULL claims before native I/O. Errors/invalid results/timeouts retain full-scan fallback and independent recipient progress. New SQLite and real- dispatcher cases cover boundaries, malformed/oversized work, ACK visibility, empty-set reconciliation and zero host/writer work while deferral is proven. See P evidence.


P/P2 verification retains the initial Linux parity failure and fixes only its post-leave lifecycle wait in P2. Both complete engineering suites pass (266/267 default tests, 190 production files, maximum 390/400). Mac unchanged-input preflight/full pass. Linux preflight passes the latency target but fails its posting window; full fails both timing targets despite complete durable audits, rotation and bounded resources. The implementation does not establish a Linux performance fix or any host admission. Exact identities/results remain in the linked P/P2 evidence.


### Bounded asynchronous native preparation (revision R, 2026-10-16)

A real-socket regression reproduces independent status reads timing out both while a recipient's fresh host verification is held and while its claim waits for an independently reserved SQLite writer. Move the existing ordinary `prepare_wake` call into the bounded native job before presentation. Every positive still performs fresh host verification, current persisted session and phase checks, an individual immediate FULL claim and commit-before-native-I/O. There is no grouped fresh claim, retained host-authority proof or schema change.

The 32 jobs now include preparation; each preparation retains a five-second deadline, separately from the five-second native presentation deadline. The 32-entry outcome queue contains actual native completions, local no-work or preparation failures. Only actual completions reach the existing ready-write engine; local failures become bounded diagnostics. Private job UUIDs prevent stale queue cleanup from removing replacement jobs, without replacing durable token/incarnation fencing. Existing leave/detach/shutdown cancellation covers both stages. Ambiguous committed claims retain ordinary orphan recovery.

Rotate the next sorted descriptor scan after the last scheduled UUID. This purely ephemeral cursor prevents repeatedly rejected roots from filling all 32 slots ahead of independent later work. It caches no eligibility and adds no timer. The conservative candidate read remains bounded coordinator work. Existing command FIFO, mixed-write groups and query/control fences remain. Verify independent read/recipient progress, no pre-commit native effect, failed-root fairness and cancellation before fresh unchanged-input workloads.


R/R2 verification retains the initial Linux direct-dispatch teardown failure; R2 waits for owned job outcomes before closing test databases, with identical product code. Both complete suites pass (280/281 default tests, 193 production files, maximum 390/400). Unchanged Mac preflight/full pass. Linux preflight/full pass latency (49.668114 / 114.604520 ms p95), but fail their fixed posting windows after complete durable audits and bounded resources. No Linux performance or host-admission completion is claimed. See source and original reports.


### Historical asynchronous advisory scans (revision S, superseded 2026-10-16)

The following implementation experiment is withdrawn by [ADR 0012](0012-macos-performance-and-runtime-simplicity.md). Current code uses R2's sequential advisory read; the bounded native preparation described above remains. Retain this section as historical rationale and evidence, not a current scheduling contract.

A separate R2 diagnostic measures 3.737 seconds of candidate-start service over one full-rate minute, while native preparation already runs outside the coordinator. Queue wait dominates individual RPC wall latency. The diagnostic is not stock performance acceptance or a causal CPU/fsync partition.

Move only the existing optional advisory read to one asynchronous job with its unchanged five-second deadline. It returns through the existing 32-entry outcome queue. Retain at most one ready result and one coalesced successor-request flag. A successful mutation/lifecycle/completion or housekeeping request arriving while the read runs requires a fresh successor snapshot; the older negative cannot consume it. No collection timer, host-phase proof or negative cache is introduced. No-port runtimes perform no scan.

The coordinator stages a scan result while collecting outcomes and advances it only after the selected ordinary/native write group returns. A read adds no scheduling request, though already owed work may advance afterward. Filter by current exact descriptors and cancellation before launching jobs. Cancellation of an old positive does not suppress a later fresh request permanently. Abort the bounded reader on runtime drop. Timeout/error retains conservative full fallback and a bounded diagnostic; only actual native outcomes enter the commit engine. Every positive still receives ordinary fresh host verification and an individual immediate FULL claim before native I/O. Command FIFO and carried query/control fences, native tokens, attempts and orphan recovery are unchanged.

Four separate real-store tests cover nonblocking scheduling/drop cancellation, old-negative coalescing across a committed post, cancelled positives followed by fresh requests, and the actual five-second timeout/fallback. Existing direct candidate tests now drive the asynchronous result explicitly; real-RPC control tests await observed idle scan/job state instead of treating a status call as a preparation fence. The original state, budget, fallback, approval and independent recipient assertions remain. Both complete engineering suites and fresh stock workloads are required before any performance claim.

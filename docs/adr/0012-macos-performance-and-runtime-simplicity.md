# ADR 0012: macOS timing requirements and simpler runtime coordination

## Status

Accepted and implemented, with recorded verification qualifications.

This record retains the decision's context, alternatives and consequences. Read it with the [current contracts](../README.md) and [verification scope](../guides/verification.md). Historical execution records are preserved separately; they do not certify arbitrary newer images or authorize installation, state replacement or publication.

## Reaffirmed maintainability policy, 2026-11-04

The user accepted the investigation's storage-synchronization limitation and reaffirmed macOS absolute timing gates with Linux numerical timing report-only. The later Linux numerical optimization goal is superseded, not achieved. Keep the same strict functional profile and raw measurements on both platforms; a false target flag remains false even when the Linux functional run exits zero. Do not introduce automatic coefficients, calibration or baseline-management infrastructure. Existing FULL durability, authority, resource/log/queue and hang/stop safeguards remain mandatory. This policy does not adopt private optimization candidates or rewrite earlier source/image-qualified results.

## Context

The native Linux executor is an Intel Mac 2020 running Arch Linux. Repeated workloads preserve complete functional accounting but miss timing targets. The minimal SeaORM/SQLite comparison shows fast memory/tmpfs cases and much slower persistent WAL/FULL commits on that executor. This localizes an observed cost to persistent synchronization; it does not prove defective hardware, general SeaORM/adapter inefficiency, or a complete explanation of application latency. See the comparison.

The user accepts the current Linux performance limitations and prioritizes maintainable code over further asynchronous scheduling for that machine.

## Decision

Keep the macOS T19 requirements: five participants, 6,000 posts in the fixed 600-second window and commit-return-to-local-dispatch p95 below 250 ms. Linux records the same measurements and numeric comparisons, without requiring either timing target to pass. Historical failed runs keep their original exit codes and target flags. An exit-zero Linux functional run is not a claim that its timing targets were met.

Both platforms still require complete event/inbox/message/read accounting, observer continuity, durable stop/reopen integrity, bounded resources and logs, normal engineering gates and real-host acceptance. FULL synchronization and the SeaORM-only boundary remain required. This exception does not admit any host version or waive G0.

Remove revision S's independent advisory-reader job, retained scan result, nonce/cancellation filtering and coalesced successor-request state. Restore the verified R2 sequential candidate read, with its five-second deadline and conservative full fallback. Remove tests that exist only for the withdrawn scanner, and restore the existing candidate/capacity tests with their original functional assertions.

Keep ordinary fresh host verification, individual FULL claim and presentation in the existing bounded native job. These waits must not monopolize independent read RPCs; separate socket tests establish that functional requirement. Keep its existing limits and leave/detach/shutdown cancellation. This does not add a second scheduling mechanism. SeaORM and socket APIs remain asynchronous Rust APIs; sequential coordination means awaiting them in order, not blocking Tokio threads with synchronous database calls.

Keep bounded set queries and already-ready transaction grouping. They use the same ordered application commit engine, add no collection timer or background queue, and preserve durability and per-request failure isolation. Removing them would be a separate policy/transaction change without a demonstrated maintenance benefit.

## Consequences and verification

The workload report includes `timing_enforced`, independently of `latency_target_passed` and `posting_window_passed`. Manual macOS profiles assert both targets after all functional audits; Linux and functional smoke profiles report them without timing assertions. Functional assertions remain active.

Run the required engineering/build/staged-install checks on both platforms and a fresh macOS full workload after simplification. Retain existing Linux full results rather than launching another tuning cycle. Record source identities, remaining limitations and outcomes in the implementation evidence.

The owned attachment heartbeat is required functional lifecycle control: one cancellable 15-second task per joined attachment invokes the shared operation and verifies current host facts. It is not a Linux timing optimization or per-message scheduler. Read checks and unbound transports create no presence renewal. Both timing-policy exceptions and actual-host proof requirements remain unchanged.

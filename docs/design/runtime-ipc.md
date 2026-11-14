# Runtime ownership and IPC

One project runtime owns its private Unix endpoint under the selected state root. An endpoint filename or lock-file existence does not prove ownership. Startup checks retained project identity and exact live kernel-owner facts; active or uncertain owners are preserved rather than unlinked or replaced.

## Routing and bounds

Typed framed requests carry stable request identities and verified selected context. The project runtime validates the current original owner, root, generation and receiving scope. Connection disappearance is not a positive native non-entry result and cannot authorize a resend.

The coordinator serializes shared application writes through the existing 64-command ingress. Native jobs, completed outcomes, observer streams, frame sizes and diagnostic queues have independent bounds. Only already-ready adjacent writes may be grouped; queries and lifecycle/control requests fence the group. Semantic failures retain independent savepoints. Infrastructure/outer-commit failure exposes no provisional successes.

Host observation and input are outside the database writer. Native preparation keeps one original five-second budget across its staged requests; cancellation prevents unapplied preparation from entering later. Actual original results remain eligible for Core validation after caller loss.

## Presence and observer ownership

Logical participation, transport attachment, effective presence and physical process lifetime are distinct. Reconciliation may suspend uncertain native work without inventing a departure. Positive original-owner loss fences that launch. Compatible executable updates do not replace original process identity.

Observers use typed snapshots and one owned reconnect/replay path. Subscription cursors advance only over confirmed application progress. Closing a subscription cancels its owned source and drains bounded resources; it does not stop the project runtime or shared native daemon. Ordinary viewer reads do not Join, mark Messages read or wake agents.

See [delivery](delivery.md), [read API](read-api.md) and [runtime diagnostics](../guides/runtime-diagnostics.md).

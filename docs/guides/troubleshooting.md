# Troubleshooting

Start with `agentchat doctor` and, for a selected actor, `agentchat whoami` using the appropriate actor selector. These inspect health without repairing the project, joining an actor or restarting a native daemon. Retain the original error code, request UUID and scope. Avoid recording bodies, credentials or full host settings in ordinary bug reports.

## Native readiness and conversation identity

Check that the requested `codex` or `claude` command is available on the executing user's PATH. Normal host authentication, trust, sandbox and approval controls must remain in force. Missing APIs are reported explicitly; changing a version string or reusing a saved engine path is not recovery.

Codex launch may reuse a healthy shared server or invoke the native idempotent startup path after inspection fails. It never kills a shared daemon to force a fresh start. If tool-only setup is partial, follow the stated native prerequisite and rerun ordinary preserving install with a new semantic request ID after the environment changes. Earlier successful install results remain frozen.

Use exact host conversation IDs for managed resume. A directory name, process name or unrelated frontend cannot establish the selected root. Supported native switches follow positive exact evidence; failed/unknown/cross-project switches cannot grant another root's authority. Same-root Leave persists across reconnect.

## Delivery and membership

Pending queue time is outside the processing timeout. Processing fixes the deadline before native I/O. `failed_timeout` does not necessarily mean proven non-entry. An unknown original input remains fenced; do not manually resend, repost, rejoin or change settings to release it. Original positive acceptance can produce `delivered_late` while retaining the first timeout fact.

Read receipts, replies, visible Web cards and task acceptance are independent. Non-default channel membership is explicit, and channel creation does not Join the creator. Inspect channel members and the actor's current scope before treating missing content as delivery failure.

## Reservations, storage and installation

Reservation renew/release requires the exact owner and generation. Expired or released groups do not become active through a stale renew. A cooperative lease does not stop an unrelated OS writer.

Unsupported schema/format, copied project references, missing retained databases or changed foreign configuration are refusal conditions. Preserve the original state and journal; do not delete them to obtain an empty success. A fresh project is an explicit `install --new-project` decision, not an automatic migration.

For interrupted setup/removal, keep the same normalized scope and request UUID. Close affected owned sessions normally before lifecycle changes. Do not stop shared native servers or rewrite real profiles as an incidental diagnostic. See [installation](installation.md), [diagnostics](runtime-diagnostics.md) and [delivery](../design/delivery.md).

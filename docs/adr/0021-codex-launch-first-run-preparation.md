# ADR 0021: Complete Codex native preparation during managed launch

## Status

Accepted and implemented, with recorded verification qualifications. Naturally stopped real-host first-start verification remains separate from the implemented path and existing-daemon observations.

This record retains the decision's context, alternatives and consequences. Read it with the [current contracts](../README.md) and [verification scope](../guides/verification.md). Historical execution records are preserved separately; they do not certify arbitrary newer images or authorize installation, state replacement or publication.

## Decision

The ordinary workflow remains `ac init` followed by `ac launch codex`. Init establishes project identity and preserving MCP/skill ownership. A successfully initialized tool-only Codex installation is a valid starting point for launch; the user need not manually start Codex and rerun init to obtain its selector.

Shared application launch first validates the project, fresh actor, intact current owned MCP/skill bundle and matching ordinary native configuration discovery. An isolated `AC_USER_CONFIG_ROOT`, stale/foreign/missing components or installer conflict is refused before host startup or configuration changes. Adding Codex after an init where its executable was absent still requires an explicit ordinary init to establish its owned tools.

Inspect the captured Codex CLI's native daemon version command. Reuse a running server only after checking its real native image and matching version. If inspection fails, invoke `app-server daemon start` once with a bounded 30-second command deadline. The user explicitly accepted delegating that decision to native idempotent start: a failed probe is not AC proof that the server stopped. Native start serializes its own reuse/start decision, including a concurrent launcher. Never substitute restart, update, bootstrap, remote-control changes, another server or configuration overrides. A failed/uncertain start result is not retried automatically. After command success, independently inspect the real image/version and required API; the return alone is not readiness.

The inspected official 0.163.0 [version implementation](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/app-server-daemon/src/lib.rs#L609) returns a running result or a probe error, rather than stopped-state JSON. Its [start implementation](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/app-server-daemon/src/lib.rs#L379) owns the serialized idempotent decision. These sources explain the selected route; they are not actual stopped-host validation evidence or a version allowlist.

An existing intact native selector must match the inspected daemon; do not silently update it. If only the selector is missing, verify the live experimental initialize API, then install the exact native executable/version through the existing preserving installer. Use a fresh internal installation request UUID, bound to the project, and select only Codex artifacts. Recheck ownership and concurrent native selection under its existing lock. Do not add/update unrelated Claude artifacts or rewrite any earlier successful init result. Re-read the owned launch plan and environment before continuing ordinary startup.

Do not hold the installer lock across native I/O. A failed configuration update may leave the shared daemon running and the existing guarded installation journal pending. Normal recovery rules apply. Cancellation or managed CLI exit does not stop a shared daemon, including one this invocation started.

Server preparation grants no participation or conversation identity. Keep the independent exact native CLI process/start identity, root ID, trust/integration handshake, pre-prompt membership, control fences and fresh launch generation. Trust, approval, sandbox, model and authentication settings remain host-owned. ADR 0020's best-effort API policy and ADRs 0018/0019's lifetime, FIFO and native acceptance boundaries are unchanged.

## Verification

Use temporary AC state/configuration and an owned native-command fixture for unavailable/running/malformed observations, startup failure/reinspection and API failure. Verify one native decision after failed inspection without restart or automatic retry, selector/foreign-setting/receipt preservation, concurrent selector refusal, isolated-root refusal before effects and repeated launch without another installation. Record actual-host reuse/start observations separately; documentation/help or fixture success alone does not establish an actual stopped-host startup result. Do not stop a real shared daemon merely to create a test precondition.

See the implementation evidence for the full-workspace checkpoint, final test-fixture formatting supplement and the explicit actual-host limits.

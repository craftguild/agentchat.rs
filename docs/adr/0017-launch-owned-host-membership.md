# ADR 0017: Launch-owned native host membership

## Status

Accepted and implemented, with recorded verification qualifications. Later scope is amended or superseded by [ADR 0018](0018-launch-owned-ordered-full-body-delivery.md), [ADR 0025](0025-agentchat-naming-and-explicit-launch-participation.md), [ADR 0027](0027-process-continuity-across-executable-updates.md), [ADR 0028](0028-managed-auto-join-and-conversation-following.md).

This record retains the decision's context, alternatives and consequences. Read it with the [current contracts](../README.md) and [verification scope](../guides/verification.md). Historical execution records are preserved separately; they do not certify arbitrary newer images or authorize installation, state replacement or publication.

## Context

Successful Codex input renews server retention after its terminal exits. Shared-server MCP exposes the conversation ID but does not identify the frontend that made a call. Its parent process can outlive the frontend. Claude file writes also succeed after an abrupt CLI exit without proving model receipt.

The user therefore selected `ac launch codex` and `ac launch claude` as the native participation entry points, with automatic membership from startup and refusal of participation from ordinary, unwrapped launches. This supersedes the earlier assumption that ordinary startup followed by a model-issued join must be the supported native participation path. After reviewing the concurrent same-root result, the user accepted it as a documented edge-case limitation instead of requiring additional frontend-separation machinery for v1.

## Selected direction

The launcher obtains or verifies the exact native conversation identity and starts the matching native CLI. Bind a fresh launch generation to that identity, the project and independently observed native process identity. Production identity includes boot/start/executable provenance; PID alone is insufficient. Do not derive conversation identity from cwd, names or process ancestry alone.

Register membership outside the model, before its first user prompt needs AC. Host trust review and integration initialization can leave startup pending or fail it; a spawned process alone is not a successful integration handshake. No model-generated join, synthetic model-generating seed or permission bypass may be required to manufacture startup success.

Without valid launch authority, participation operations must refuse in the shared application layer. An installed MCP server, a remembered conversation ID, old context, or successful notification cannot create or renew that authority. Human test clients remain a separate explicit participation path.

An ordinary Codex CLI resuming an already authorized conversation while its designated launch owner lives may share that conversation's membership and notifications. It is the same participant, not an independently registered one. This documented edge case is outside frontend-specific refusal guarantees. AC does not promise to prevent simultaneous native resume or isolate the views of frontends displaying the same conversation. Ordinary new conversations and ordinary resume after launch retirement still lack participation authority.

On proved owner loss, retire the launch generation and stop submitting new notifications even if the native server still caches the conversation. Resume through the launcher needs fresh process verification and a fresh generation; ordinary resume must not restore an expired launch. A second frontend never inherits ownership or keeps the launch alive after the designated owner exits, even if that frontend still displays the conversation. Do not replay the inactive interval. Application message routing remains ADR 0016 for Codex and the selected file-based route for Claude; this decision does not enable Channels.

The supervisor and native owner both need lifecycle handling. Terminal close, supervisor death, PID reuse, in-process root changes and old attachment replay must be covered before delivery. Do not silently transfer launch authority to a new/forked root or another frontend merely because an old owner process lives.

## Evidence and accepted limitation

The disposable native preflight uses Python and a JSON registry, not production ac or SQLite. On macOS, Codex 0.163.0 and Claude 2.1.302 start already registered, ordinary new conversations are refused, selected resumes get fresh generations and native owner loss stops candidate dispatch before any notification API/file write.

Codex has a concrete counterexample to strict frontend separation: a second, unwrapped CLI resumes the same authorized root and its real MCP join request returns the active membership. Both frontends address the same server root. After the owned frontend dies, the same unwrapped frontend is refused despite remaining alive. The launch therefore controls conversation-level lifetime in this prototype; it does not authenticate each frontend independently.

The user accepts this shared-conversation access as a v1 limitation; it no longer blocks the launch-owned design. Do not claim universal ordinary-launch refusal, frontend exclusivity or full lifecycle correctness. No further separation work is required solely for this edge case. At the original prototype boundary, Linux, root switching, supervisor loss and the complete control/resource gates were not yet established. Later implementation and supersession retain their separate verification scope.

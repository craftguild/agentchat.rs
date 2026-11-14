# ADR 0020: Best-effort API-surface host compatibility

## Status

Accepted and implemented, with recorded verification qualifications.

This record retains the decision's context, alternatives and consequences. Read it with the [current contracts](../README.md) and [verification scope](../guides/verification.md). Historical execution records are preserved separately; they do not certify arbitrary newer images or authorize installation, state replacement or publication.

## Context

Codex and Claude communication boundaries are experimental and change often. An exact semantic version does not prove compatible behavior, while a different version can retain every API used by Agent Chat. Maintaining a release allowlist therefore rejects potentially usable hosts without providing a strong guarantee.

Agent Chat does not currently need to certify host behavior. The intended policy is practical and best-effort: launch when the adapter can find the API surface it uses, preserve runtime safety boundaries, and report an ordinary compatibility error if a required API is absent.

## Decision

Each host adapter declares the minimal API surface it consumes. Compatibility is checked incrementally against the live host as those APIs are used. Agent Chat does not add executable fingerprints, compatibility attestations, behavioral certificates or an exact-version positive allowlist.

For Codex, the surface includes experimental initialization and the app-server methods used for thread creation, resume, observation, MCP health and delivery. For Claude, it includes the native plugin session, command, process and environment functions used by the installed adapter and the lifecycle events it registers. Some surfaces are observable only after the host starts; that is an accepted limitation of best-effort launch.

The reported host version is retained in diagnostics and evidence. A selected executable, daemon, initialized server, frontend and runtime context must still report the same version during one launch. This equality prevents two different host instances from being mixed; it is not compatibility admission. Known-bad versions may be refused with a documented reason, but an unknown version is not refused merely because it is absent from a list.

Missing required APIs reached through an AC operation return `unsupported_protocol`. The error includes the host, observed version when available and the missing method or pointer; CLI and MCP expose the same error. Claude's native analyzer requires SDK methods to remain literal calls, so the adapter cannot reflect over those methods as values. It checks each pointer when the corresponding literal call is made. APIs needed to create participation are checked before that participation is committed; later-only APIs fail at their first use. A missing pointer blocks the affected prompt or operation with `Agent Chat native capability unavailable` and its fixed API name. The bundled `ac` skill must present either error form to its caller and must not hide it, translate it into an exact-version failure or attempt an unsafe workaround.

Existing root, owner, process, generation, FIFO, pause, approval, trust and sandbox checks remain runtime requirements. Platform-specific implementation limits may still refuse launch where the adapter cannot be built or operated. They are not expanded into an OS-version compatibility certification scheme.

G0 remains the release-quality real-host validation matrix and the basis for a claim that an exact environment was tested. It is no longer a certificate that must exist before an ordinary best-effort launch can be attempted. Documentation must distinguish `tested` from `best_effort` and must not describe API presence as proof of model receipt or behavioral equivalence.

## Consequences

Host upgrades that retain the required surface can work without an Agent Chat release. A surface-preserving semantic change may still fail at runtime; this is accepted and must produce a bounded, visible error. New or changed adapter needs are added to the declared surface and corresponding error tests.

Initialization remains preserving and rerunnable. When discovery observes a new live host selection, it may update only Agent Chat-owned selector fields while retaining unrelated host configuration. An intact selector is configuration, not proof that its host is still running or compatible.

Tests cover a changed version string with the required API, a missing required API with a named public error, selector/live version disagreement and CLI/MCP error parity. Real-host evidence records exact versions for reproducibility without turning those versions into the positive admission key.

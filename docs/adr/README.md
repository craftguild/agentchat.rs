# Architecture decisions

These numbered records retain accepted decisions and their rationale. Later explicit amendments supersede the indicated portions of earlier records; an older decision is not a current requirement merely because it remains here. The [current design](../design/README.md) describes the composed product.

| ADR | Decision | Status |
| --- | --- | --- |
| [0001](0001-project-runtime.md) | Use a lazy runtime per project | Implemented with qualified verification; later scope: 0018 |
| [0002](0002-project-identity-and-storage.md) | Separate project identity from paths and use SQLite | Implemented with qualified verification |
| [0003](0003-broadcast-mentions-and-receipts.md) | Broadcast information but wake only mentioned recipients | Implemented with qualified verification; later scope: 0029, 0032 |
| [0004](0004-cooperative-glob-leases.md) | Use cooperative reservations with exact glob overlap | Implemented with qualified verification |
| [0005](0005-versioned-host-adapters.md) | Admit exact CLI versions only after real-host validation | Implemented with qualified verification; later scope: 0020 |
| [0006](0006-rust-boundaries-and-source-size.md) | Use responsibility-based Rust crates and code-line limits | Implemented with qualified verification |
| [0007](0007-seaorm-only-persistence.md) | Use SeaORM for all database queries | Implemented with qualified verification |
| [0008](0008-recipient-warnings-and-read-deadlines.md) | Preserve posts and escalate unconfirmed reads to their author | Implemented with qualified verification; later scope: 0030, 0032 |
| [0009](0009-make-build-and-install.md) | Make build and executable installation | Implemented with qualified verification |
| [0010](0010-clap-and-cli-banner.md) | Clap entry points and an embedded CLI banner | Implemented with qualified verification |
| [0011](0011-bounded-mutation-group-commits.md) | Share durability across bounded ordered mutation groups | Implemented with qualified verification |
| [0012](0012-macos-performance-and-runtime-simplicity.md) | macOS timing requirements and simpler runtime coordination | Implemented with qualified verification |
| [0013](0013-public-initialization-tool-installation.md) | compose preserving tool installation into public initialization | Implemented with qualified verification; later scope: 0026 |
| [0014](0014-read-only-installation-health.md) | current installation health without repair | Implemented with qualified verification; later scope: 0031 |
| [0015](0015-root-scoped-mcp-attachments.md) | immutable root attachments behind a retained MCP transport | Implemented with qualified verification; later scope: 0025, 0028 |
| [0016](0016-codex-serial-full-body-delivery.md) | Serialize direct full-body delivery to Codex | Implemented with qualified verification; later scope: 0018, 0022, 0032 |
| [0017](0017-launch-owned-host-membership.md) | Launch-owned native host membership | Implemented with qualified verification; later scope: 0018, 0025, 0027, 0028 |
| [0018](0018-launch-owned-ordered-full-body-delivery.md) | Launch-owned, ordered full-body delivery | Implemented with qualified verification; later scope: 0025, 0027, 0028, 0032 |
| [0019](0019-host-owned-delivery-ack-and-agent-roundtrip-reduction.md) | Host-owned delivery ACK and fewer agent round trips | Implemented with qualified verification; later scope: 0030, 0032 |
| [0020](0020-best-effort-api-surface-host-compatibility.md) | Best-effort API-surface host compatibility | Implemented with qualified verification |
| [0021](0021-codex-launch-first-run-preparation.md) | Complete Codex native preparation during managed launch | Implemented with qualified verification; real stopped-host first-start observation remains open |
| [0022](0022-host-adapter-boundaries-and-agent-readable-input.md) | Keep host mechanisms behind common delivery contracts | Implemented with qualified verification |
| [0023](0023-typed-shared-read-results.md) | Preserve typed shared read results across transport boundaries | Implemented with qualified verification |
| [0024](0024-local-read-only-web-viewer.md) | Serve a local read-only GraphQL chat viewer from the agentchat binary | Implemented with qualified verification; later scope: 0032 |
| [0025](0025-agentchat-naming-and-explicit-launch-participation.md) | AgentChat naming, explicit participation and compact input framing | Implemented with qualified verification; later scope: 0027, 0028, 0029 |
| [0026](0026-install-uninstall-and-global-reset.md) | Explicit installation, scoped removal and global reset | Implemented with qualified verification |
| [0027](0027-process-continuity-across-executable-updates.md) | Process continuity across executable updates | Implemented with qualified verification |
| [0028](0028-managed-auto-join-and-conversation-following.md) | Managed auto-join and conversation following | Implemented with qualified verification |
| [0029](0029-autonomous-collaboration-and-channel-scopes.md) | Autonomous collaboration, channel scopes and archive classification | Implemented with qualified verification |
| [0030](0030-project-receipt-policy-and-delivery-confirmation.md) | Project receipt policy and standard delivery confirmation | Implemented with qualified verification; later scope: 0032 |
| [0031](0031-cli-command-and-actor-interface.md) | CLI commands, shared APIs and explicit operation actors | Implemented with qualified verification; later scope: 0032 |
| [0032](0032-delivery-state-machine-and-ack-removal.md) | Enum delivery state machine and removal of ACK | Implemented with qualified verification |
| [0033](0033-craftguild-build-and-release.md) | Shared CraftGuild build, quality and initial release contract | Accepted for implementation |

The current delivery contract is ADR 0032: immutable Message/Notice Dispatch Headers, recipient Details, append-only transition history, processing deadlines, ACK-free native settlement and independent reads. Earlier ACK/confirmation records preserve historical rationale, not a public operation to restore.

The Viewer uses React and a catalog-generated SDK. Managed startup and supported conversation following use the current exact-owner lifecycle rules. Source implementation, observed verification and operational installation are distinct. See [verification](../guides/verification.md) for qualifications and outstanding public-build/real-host limits.

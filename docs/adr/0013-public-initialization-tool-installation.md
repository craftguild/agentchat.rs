# ADR 0013: compose preserving tool installation into public initialization

## Status

Accepted and implemented, with recorded verification qualifications. Later scope is amended or superseded by [ADR 0026](0026-install-uninstall-and-global-reset.md).

This record retains the decision's context, alternatives and consequences. Read it with the [current contracts](../README.md) and [verification scope](../guides/verification.md). Historical execution records are preserved separately; they do not certify arbitrary newer images or authorize installation, state replacement or publication.

## Decision

CLI `ac init` and MCP `ac_init` invoke the same `Operations` workflow. The executable composition root supplies captured native environment paths, PATH, the ac state root and its absolute executable. Only initialization uses them; construction, ordinary queries and MCP startup do not install anything.

After durable project initialization, `ToolSetup` selects `codex` and `claude` executable files from the captured PATH. Presence ordinarily authorizes only installation of their existing MCP/skill artifacts. Missing hosts report `not_installed`; installed tools report `tools_installed`, with overall state `partial`.

When `AC_USER_CONFIG_ROOT` is absent, public setup may spend at most five seconds inspecting the already-running shared daemon and verifying the native executable/version it reports. A match installs that exact selector and reports `native_gateway` plus lifecycle `available`. Failure falls back to tool-only setup. This starts no daemon/model and changes no approval, trust, sandbox, model or collaboration setting. Isolated roots do not enable the branch by default.

Init's tool-only outcome does not imply managed-launch readiness. Under ADR 0021, launch performs its own bounded native preparation with a fresh installation key; a successful old init request remains immutable. Manual recovery can still start Codex normally and explicitly rerun ordinary init with a new request ID. No path changes host trust or approval. See the onboarding correction.

Use both hosts' resolved allowed roots consistently, even if only one host is currently present. Exact artifact selectors still restrict ownership. This keeps the ownership ledger compatible when another host appears later. Current host availability never removes earlier owned components.

For isolated native verification, a `CLAUDE_CONFIG_DIR` contained in an explicit `AC_USER_CONFIG_ROOT` retains Claude's native directory layout, including the registry inside that directory. Capture the same directory in the generated Claude MCP entry; omit it from Codex's environment. Outside ambient overrides remain ignored and the default sandbox layout and ownership-root namespace are unchanged. Nested allowed roots resolve through their outer root alias, so an interior symbolic link cannot escape installation confinement. Existing ownership checks still refuse destination changes/adoption. This closes the earlier explicit-library-root/public-CLI mapping gap without changing HOME/CODEX_HOME or admitting a host.

The existing installer owns one OS lock and executes one blocking filesystem worker. Reuse its complete-plan validation, preserving merge, journal and durable publication implementation; add no asynchronous scheduler or second write engine. Only after installation completes, save a private bounded receipt at `AC_HOME/integrations/requests/<request-id>.json`. Version 2 also stores the complete successful component/lifecycle response, captured under the installer lock after its owned ledger is durable. Its version, request UUID, project UUID, canonical unique host set and response shape are validated on replay. The project workflow first checks the original normalized actor/arguments. A successful setup replay returns its original host/component report without discovering new hosts or reapplying an old binary/configuration plan.

The user approved the legacy compatibility policy on 2026-10-19. Version 1 receipts stored only the host set, so their original detailed response cannot be reconstructed. Exact-key replay returns `configuration_conflict` with `reason=historical_installation_result_unavailable`, before discovery, journal recovery or host configuration writes. The receipt and existing project remain intact. Callers can use `status` for current health or explicitly run ordinary `init` with a new request ID. Never automatically repeat `--new-project` or `--relocate`. This error does not establish that the original installation failed. Version 2 replay returns its saved response before discovery or current health inspection; later selector changes or foreign edits cannot rewrite history.

If installation fails, keep the project and its initialization receipt. Return the normal error envelope with the project UUID plus separate `details.initialization` and `details.integrations` outcomes. Retrying the same key after a corrected collision resumes setup without allocating another project. A crash after installation but before its request receipt is handled by the existing preserving installer on retry. Completed replay is historical operation evidence, not a fresh configuration-health check.

## Verification and remaining work

Exercise actual CLI/MCP subprocesses under temporary ac/config roots for zero, one and both hosts, concurrent initialization, frozen successful retries and collision recovery. Test host addition, binary updates and metadata corruption without executing host fixtures. Real child-process exits span project receipt, installer validation/journal, both files, ownership and completed installation.

The original decision completed public MCP/skill setup composition only. The later Mac Codex candidate adds exact selector discovery and selected public-path evidence. Current public discovery and installed CLI/SDK paths on both platforms are recorded in the final checkpoint and the no-helper Claude cases. Their scopes remain distinct from complete G0/G4 release verification.


## Native discovery amendment

The shared preserving setup now includes the first-time Claude native selector branch alongside Codex. It requires a bounded native-image/version observation, preserves existing owned selections and foreign policy, and installs the same module/hook/MCP bundle used by managed startup. Isolated public setup still disables ambient discovery. Component tests verify preservation, retry/precedence and refusal. Actual first-install public controllers pass on Mac 2.1.303 and Linux 2.1.302; `.10.7` is closed for that scope. Selection and installation never prove a live conversation or trust.

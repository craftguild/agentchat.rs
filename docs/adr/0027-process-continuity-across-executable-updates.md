# ADR 0027: Process continuity across executable updates

## Status

Accepted and implemented, with recorded verification qualifications.

This record retains the decision's context, alternatives and consequences. Read it with the [current contracts](../README.md) and [verification scope](../guides/verification.md). Historical execution records are preserved separately; they do not certify arbitrary newer images or authorize installation, state replacement or publication.

## Context

A user reported `session_required` after replacing AgentChat using `sudo make install` during a managed Codex session. Source inspection identifies a plausible route: process identity includes executable path/device/inode, while installation replaces the file. On macOS, the observation obtains the process's executable path and then reads that path's current filesystem metadata. The reported live process was not independently compared with its retained token; this is a source-grounded diagnosis, not a completed incident reproduction.

The launch guard combines original-process continuity with mutable installed-file identity. Replacing the installed binary does not itself establish that the original managed process exited. Requiring image equality for continued delivery therefore obstructs ordinary updates without serving the intended lifetime rule.

## Decision

### Resolve new host launches from PATH: 2026-10-29 amendment

The user additionally requires both `codex` and `claude` managed launch to find the current command on the executing user's PATH. Missing/inaccessible commands return an error, with no fallback to an old installed engine. Persistent owned configuration selects AgentChat's stable dispatcher, not a version directory. Codex still discovers and checks its matching native shared-server endpoint; PATH selection does not authorize restarting a server or bypassing its API and original-parent checks.

Claude records the chosen version and independently captured original native owner/supervisor in a private immutable launch snapshot. New native helpers use that same snapshot and verified ancestry; they never rediscover another engine for an existing owner. Initial executable admission remains strict. Subsequent continuity does not require the engine's old file to remain installed. Native SDK root/control challenges, publication, participation, receipt and unknown-head fences are unchanged. A snapshot alone grants no conversation authority.

The current delivery mechanism is retained. The Channels research-preview probe is not product adoption or a replacement receipt certificate. This amendment is implemented; its focused/current preview evidence is separate from the completed historical ADR0027 campaign. Legacy owned bundles require an explicit preserving update; ordinary launch performs no host-settings repair.

The focused regression mapping is:

- [Claude upgrade tests](../../crates/agentchat-hosts/tests/claude_upgrade.rs): PATH A/B selection, removal of A, the exact original helper/namespace, wrong root/generation, original exit and missing-command refusal; stable artifacts contain no engine path/version.
- [Codex startup tests](../../crates/agentchat-hosts/tests/unit/codex_startup.rs): each caller's current PATH, unchanged owned configuration, missing-command refusal, and retained native shared-server/control checks.
- [Preserving setup tests](../../crates/agentchat-hosts/tests/unit/claude_setup_workflow.rs): explicit legacy-bundle migration, absent old engines, foreign configuration retention, immutable completed requests and refusal of foreign selector edits.
- [Launch collection tests](../../crates/agentchat-hosts/tests/unit/launch_reaper.rs): preserve live or foreign snapshots and remove only after positive original native birth loss, within the existing bounded maintenance budget.
- [Process continuity tests](../../crates/agentchat-hosts/tests/process/continuity.rs): retain boot/start/user, uncertainty and original-owner loss boundaries.

These checks do not certify an unexecuted real-host preview or transfer the historical both-platform/native/workload results to the new whole binary.

### Separate startup validation and ongoing continuity

Initial managed startup continues to validate the selected executable, actual spawned CLI, original supervisor and exact host conversation through the existing trusted startup/readiness route. Native MCP caller validation and native API capability checks retain their separate responsibilities. A directory, display name, arbitrary PID or supplied token cannot establish managed participation.

For ongoing continuity of both the designated native CLI and AgentChat supervisor, use the original kernel PID, kernel process-start identity, OS boot identity and user identity. Require a live process with matching supported user credentials. Do not use executable inode, device or mutable executable-path equality as a continuation condition on either platform. A filesystem lookup failure caused by installed-file replacement must not remain an indirect image guard. Removing only one comparison field is insufficient if such dependencies still suspend participation.

Keep the exact conversation/project binding, original owners, launch generation and participation generation independent of this liveness observation. Matching process facts never authorize another conversation, transfer ownership to a surviving frontend, create a context or perform Join.

Positive exit, boot/start replacement or PID reuse retains the existing owner-loss retirement and recipient-backlog rules. Unreadable, malformed, racing or user-inconsistent observations remain uncertain: suspend delivery and preserve recoverable state rather than declaring death or accepting guessed authority.

A same-PID, same-start `exec` is not guaranteed to be detected by this continuity check. This is an accepted boundary of the selected process-lifetime contract; startup validation, exact routing and host permission checks remain separate.

### Update behavior

For a compatible update, replacing the installed AgentChat executable alone must not invalidate active membership, change its generation/context, require rejoin or stop delivery. Preserve recipient acceptance order, host-owned ACK, explicit read/task separation and unknown-submission recovery without replaying delivered work. Both Codex and Claude retain their normal host controls.

Running processes continue their loaded code. Newly started processes and on-demand helpers may use the replacement image. Validate the resulting mixed CLI/runtime/MCP/Claude-hook operation; process continuity alone does not establish protocol or storage compatibility. Incompatible changes must fail clearly before unsafe mutation and may require a deliberate restart. This decision promises neither arbitrary cross-version compatibility nor in-process code replacement.

Keep the ordinary single-binary command and atomic executable installation model. A communication proxy, version-directory deployment scheme, heartbeat or new scheduling mechanism is not a prerequisite for this change.

### Boundaries and migration

Accepted release inspection correction, 2026-10-30: a release AgentChat CLI may run from a different path from the owned installed MCP/hooks. Use the current ownership-verified registered command for each host when rendering desired artifacts for read-only launch/health inspection. Do not compare that saved path with the initiating release CLI path. Debug builds keep the original strict comparison to prevent development-image mixing. The build switch is Rust `debug_assertions` (enabled in ordinary debug, disabled in ordinary release); no environment/CLI bypass is introduced.

Read each saved command through the existing locked owned-selector hash check, require a valid absolute command path, and then retain exact complete artifact inspection against that path and selected state. Foreign/modified/missing selectors, modules/hooks/skills, configuration discovery and pending installs still refuse. Explicit install continues to plan against the initiating CLI and can update dispatch only through its ordinary authorized mutation. Native image and process/root/generation/readiness/trust/receipt authority are unchanged. This intentionally permits installed dispatch and caller images to differ; path equality was never a protocol/behavior compatibility certificate. Existing live compatibility/readiness checks remain required. Map both build modes to `codex_launch_plan.rs` and `native_launch_plan.rs`, including read-only health, per-host saved dispatch and modified-owned-value refusal cases. The focused release-path evidence records debug/release checks on both OSes, read-only real-profile selection and a release preview without operational adoption or a new final certificate.

Accepted startup correction, 2026-10-30: on macOS and Linux, filesystem device numbers are observations, not native startup or routing conditions. A reboot/remount can renumber the same filesystem while preserving its files. Compare retained metadata parent/file inodes and initial executable path/inode without requiring device equality. Apply the same rule to Claude's durable signal anchor and genesis identities. Keep stored device fields and their checksums for wire compatibility; old records need no destructive reset, device-number rewrite or database migration. This deliberately removes device numbers as a way to distinguish equal inode numbers on different filesystems. Original parent FDs, exact paths/names, content/preimage checks, private UID/mode, no-follow and single-link checks remain applicable. Boot/PID/start/user, current/unknown-owner refusal, root/generation and delivery fences are unchanged. Focused mapping: `native_metadata.rs`, `codex_direct_registry.rs`, `native_signal.rs` and `process_identity.rs` device-renumbering cases, with the existing file-substitution, alias, owner-loss and continuity refusal controls. Actual reboot and operational adoption are separate from simulated renumbering. The focused evidence records both-OS checks, original failures and a runnable development preview.

Retain inode and preimage checks for lifecycle fences, activity locks, journals, configuration publication, metadata provenance and deletion targets. Those checks protect filesystem objects; they are not the executable-image continuation gate being removed. Do not weaken ADR 0026 removal authority or cleanup recovery.

No public CLI/MCP wire or database-schema change is required. Preserve read compatibility and authenticity checks for valid existing launch records; recorded image fields may remain as startup/diagnostic metadata without becoming ongoing liveness conditions. Never adopt copied records or manufacture replacement owners.

The continuity guarantee starts with sessions running the new policy. Existing old supervisors, gateways or hooks cannot retroactively change their loaded checks. First adoption requires closing affected old managed sessions/runtime, refreshing integration code as required, and starting a fresh managed generation; an existing conversation may be resumed explicitly and joined. Subsequent compatible executable replacements must preserve that new-policy session.

## Validation and consequences

R32/T30 in [acceptance](../guides/verification.md) requires its own implementation evidence:

- On macOS and Linux, manage and explicitly join each native host, then replace the disposable installed executable while the session remains running. Verify unchanged original process/binding/generation and continued bidirectional complete-body delivery without rejoin.
- Cover idle and processing boundaries, repeated replacement and subsequent helper startup. Verify FIFO, native ACK/read separation and recovery without update-induced loss, duplicate acceptance or stale replay. Use a fresh native message after replacement, not merely a cached status response.
- Retain exit, PID/start/boot replacement, user mismatch, observation uncertainty, wrong-conversation and stale-generation controls. Keep unmanaged Join refusal and original-owner loss despite a surviving same-UUID frontend.
- Confirm ordinary compatible replacement works without immutable version paths, and that incompatible component/storage boundaries fail explicitly. First adoption from old loaded code is a separate restart, not a live-upgrade pass.
- Revalidate affected ADR 0026 filesystem protections; executable continuity must not make replacement lock files, configuration preimages or removal targets acceptable. Preserve the existing CLI/MCP shapes and valid-record compatibility.

All update/destructive tests use disposable executables and explicit temporary `AGENTCHAT_HOME` and `AGENTCHAT_USER_CONFIG_ROOT`; never substitute `HOME` or `CODEX_HOME` or replace a real running installation as a test. Run focused checks during development and group affected final platform verification. Existing native/workload evidence keeps its exact scope and ADR 0012 platform policy. Design acceptance and documentation checks prove no live-update behavior.

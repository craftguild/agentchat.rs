# Host and platform compatibility

AgentChat supports macOS and Linux routes for Codex CLI and Claude Code. Ordinary launch checks the actual command and required live API surface instead of relying on an exact-version allowlist. Missing capabilities return explicit errors; a known version string alone is not behavioral evidence.

## Native routes

Codex uses its shared app-server APIs and a byte-transparent managed lifecycle proxy. Observation retains exact connection/request/root correlation while native transport continues unchanged. Uncertain lifecycle evidence suspends AgentChat delivery, not arbitrary native operation. Startup checks the actual image, API and immutable original physical readiness.

Claude uses owned MCP and native hooks/modules. Current helper/owner facts must match the managed launch, project and root. Updating a native engine or AgentChat file does not hot-upgrade already loaded modules or grant another process authority.

Same-project supported conversation switches follow only verified foreground results. Cross-project/failed/unknown results cannot activate a new scope. Process continuity across compatible executable replacement uses boot/PID/kernel-start/user; initial image validation and filesystem no-follow/private-owner guards remain.

## Evidence limits

Actual-host, version-specific, synthetic fixture and source-only checks have different scope. A synthetic full-body workload excludes model inference and normal global host discovery. Compatible-update observations do not prove every future protocol or a reboot/remount matrix. Naturally stopped first-launch verification remains separate from reuse of an already running daemon.

Keep host trust, sandbox, approval and authentication controls intact. Do not copy credentials or stop shared daemons to manufacture a compatibility case. Use the [native acceptance guide](native-acceptance.md) and record exact versions, source/image identities, original failures and owned cleanup for each claimed run.

# ADR 0026: Explicit installation, scoped removal and global reset

## Status

Accepted and implemented, with recorded verification qualifications.

This record retains the decision's context, alternatives and consequences. Read it with the [current contracts](../README.md) and [verification scope](../guides/verification.md). Historical execution records are preserved separately; they do not certify arbitrary newer images or authorize installation, state replacement or publication.

## Context

Deleting `~/.agentchat` leaves repository-local project references and user-wide host configuration behind. Ordinary `init` then correctly refuses to reuse a reference whose authorization/storage is missing. Explicit `--new-project` can create fresh project storage, but the integration installer still refuses an existing MCP member when its ownership record has disappeared. An exclusive skill directory and native hooks can cause subsequent collisions as well.

These guards protect unrelated settings, but a user currently has to coordinate manual edits across project references, shared state, MCP registrations, skills and Claude hooks. A repeatable removal path is needed even after management records have been lost. Project reset and user-wide reset must have distinct, visible data-loss scopes.

## Decision

### Command responsibilities

| Command | Result |
| --- | --- |
| `agentchat install` | Create or reuse the selected project and install all AgentChat host integration components needed to start using it. |
| `agentchat install --new-project` | Explicitly create a fresh project UUID/database and replace the effective project reference, then install integrations. |
| `agentchat uninstall` | Remove the selected project's effective reference directory and its authorized per-UUID state; retain user-wide integrations and other projects. |
| `agentchat uninstall --global` | Remove AgentChat's user-wide integrations and the entire selected state root; leave every project-local reference in place. |

Replace the advertised public CLI command `init` with `install`. Do not maintain a second initialization policy behind the old name. Update current help, skills, recovery guidance and command examples at implementation cutover; historical evidence need not be rewritten. Carry forward repository/directory scope, explicit relocation, exact request IDs and existing initialization safeguards. `install` neither joins an agent nor starts a native conversation.

`make install` continues to copy the prebuilt executable. `agentchat install` configures project storage and host integration. Neither removal command deletes the installed executable, native Codex/Claude executables or build caches.

### Project-local removal

Discover the effective repository or directory scope using the same canonical rules as installation. A repository reference lives in `<git-common-dir>/agentchat`; a directory reference lives in `<root>/.agentchat`. Linked worktrees share repository scope, so this removal affects their shared AgentChat namespace. Do not recursively search for or remove other overrides, nested repositories or unrelated reference directories.

Remove the selected reference directory and its matching `AGENTCHAT_HOME/<project_id>` state, including the database, project binding, contexts and project runtime artifacts. Remove or retire associated retry records so an old successful install key cannot resurrect the removed namespace or report it as installed. Keep other project UUIDs and global integration ownership/configuration intact.

Reference-directory authority covers only recognized reference metadata (`project.json` and `init.lock`). Unrecognized members or overlap with another state root or a host integration/profile destination cause a preserving conflict before deletion. Selected state roots must also be disjoint from host integration artifacts before creating a lifecycle fence there.

Validate UUID, scope and canonical root before deleting present project data. A copied reference must never authorize removal of another root's data. If the namespace is already absent, local removal can still remove its valid stale reference. If present data lacks sufficient binding authority, preserve it and report a precise conflict rather than guessing. With no effective reference, report already absent without scanning all UUID data for a path resemblance.

### Global removal

Global means the explicitly selected AgentChat state/configuration roots for this user; it does not mean every user account or every native host profile. Resolve `AGENTCHAT_HOME`, host overrides and `AGENTCHAT_USER_CONFIG_ROOT` consistently with installation. The default state root is `~/.agentchat`.

Remove the following AgentChat components where present:

- Codex's `mcp_servers.agentchat` registration and shared `agentchat` skill.
- Claude's `mcpServers.agentchat` registration, shared skill/native plugin bundle, and exact AgentChat hook entries, including FileChanged, Stop and UserPromptSubmit integrations selected by the installed adapter.
- Other exact current AgentChat native selectors/modules owned by this product, plus its global installation records and runtime coordination state.
- The entire selected state root, including **all projects' databases/history**, bindings, contexts, receipts and integration management records.

Keep unrelated MCP entries, hooks, settings, authentication, trust, sandbox and approval controls unchanged. Do not delete shared Codex/Claude configuration files or their parent directories wholesale. The legacy `ac` namespace remains subject to ADR 0025's separate deliberate retirement; matching a broad name prefix is not removal authority.

Do not traverse repositories to update or delete their `.git/agentchat` or directory-scoped references. A leftover reference is expected after global reset. Reinstallation from it uses `agentchat install --new-project`, or an explicit local `uninstall` followed by ordinary `install`. Ordinary `install` must keep refusing a missing retained namespace and explain these recovery choices; it must not silently create empty history under the old UUID.

### Lost ownership and targeted recovery

An explicit uninstall is the recovery authority for removing identified AgentChat integration artifacts when the installation ledger is missing. A missing ledger is not, by itself, a reason to refuse cleanup. Construct a bounded removal plan from the exact supported MCP keys, skill/plugin locations and recognizable AgentChat native commands. Do not treat every hook in an event, every skill, arbitrary executable basename matches or unknown files as owned. Well-formed hooks explicitly selecting a different absolute state root remain outside this removal, including legacy `ac` hooks with shared internal command words. This preserves the separate old-namespace retirement policy.

Use available ownership records to validate the plan. When records are absent, identify each intended component from its exact destination/selector and AgentChat-specific content. Preserve ambiguous or foreign content and report the affected component and destination without exposing configuration secrets. Delete only matching members of mixed hook arrays; retain unrelated members.

Before edits, privately back up affected configuration preimages and removed AgentChat skill/module files outside the deletion set. Report the backup location; do not log preimage contents. This configuration recovery backup is not an automatic backup guarantee for all deleted chat history. Report the project/all-project data scope explicitly in the operation result.

`install` remains an ownership-preserving setup operation, not a hidden force overwrite. A foreign/unowned collision should direct the user to the explicit removal/reinstall workflow. Do not bypass native host approvals or adopt unknown configuration merely because a new project UUID was requested.

### Idempotency, activity and recovery

Already-absent targets are successful no-ops. A failed/partial removal must be safe to retry without removing unrelated settings or republishing stale setup. Serialize install, removal and runtime startup with bounded OS-held locking; deleting the state root must not invalidate the coordination fence and permit a concurrent replacement lock/start. Recheck path identities and destination preimages before publication. Reject symlink escapes, path traversal, overlapping deletion plans and dangerous roots such as `/`, the user home or host config roots. An enclosing removal also refuses nested lifecycle/recovery directories from another state root; its stable coordination and retired request history must remain outside every deletion target. Select disjoint state roots.

Before deleting live state, require affected AgentChat runtimes, viewers and managed launch sessions to be quiescent. Return a bounded actionable busy/ conflict result for live or uncertain ownership. Do not kill native coding-agent terminals or their shared servers as an implicit side effect of uninstall. Missing installation records or an ordinary native CLI process alone are not evidence of an active managed session and must not prevent lost-ledger recovery.

Keep the operation journal and restoration metadata outside a root that the operation will delete, until completion is durable. Recover interrupted work under the same deletion scope and request identity. Report completed and remaining components when cleanup cannot finish; do not claim global success while a required integration remains. An installer retry receipt predating a completed removal cannot recreate the retired state or reverse a later reset.

### Application and transport boundaries

Implement removal planning, identity checks and outcomes as common application/ runtime operations, with host adapters/installer code providing exact component selectors and filesystem/configuration mechanisms. CLI owns argument parsing and presentation, not deletion policy. Preserve the existing responsibility-based crate boundaries and SeaORM-only database access. No schema reset or migration history rewrite is required for these lifecycle commands.

The requested rename is a CLI change. Keep the existing `agentchat_init` MCP wire entry and result shape as a compatibility adapter to the shared installation operation; update its description to explain the public `install` workflow. This decision does not implicitly add destructive MCP tools. Any later exposure must use the same scoped application policies and an explicit interface decision.

## Consequences and validation

Users can reset one project or all AgentChat state without manually coordinating host configuration. Global removal deliberately makes retained local references stale. Backups, explicit new-project reinstallation and actionable diagnostics make that state understandable without weakening normal installation checks.

Requirement R31 and scenario T29 in [acceptance](../guides/verification.md) cover the command cutover, both scopes, lost-ledger cleanup, negative ownership cases, partial failure/retry, competing operations and reinstall. Use temporary `AGENTCHAT_HOME` and `AGENTCHAT_USER_CONFIG_ROOT` for every destructive test; never substitute `HOME`/`CODEX_HOME` or use real project/configuration data.

Run focused tests during development, then the affected macOS/Linux engineering, installation and lifecycle gates together. Preserve ADR 0012's platform policy and reuse unchanged native/model/workload evidence under its original scope. Design acceptance alone proves no removal behavior or operational adoption.

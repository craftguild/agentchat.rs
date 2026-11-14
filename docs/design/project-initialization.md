# Project discovery and initialization

## Namespace and discovery

The state root is `AGENTCHAT_HOME`, defaulting to `~/.agentchat`. Construction and read-only discovery do not create directories. Explicit test roots never require substituting HOME or CODEX_HOME.

Canonical directory discovery rechecks repository hints on every operation. Ordinary non-repository paths avoid Git; possible Git metadata delegates to the authoritative worktree/common-directory interface under normal Git trust rules. Directory overrides stop at repository boundaries. Linked worktrees share the common repository reference. Nested and bare repositories do not inherit an outer project's reference.

A reference contains a version, UUID and scope. The state namespace also retains canonical location authorization as native path bytes. Copying a reference to another location fails that authorization check. Relocation requires explicit intent and the original canonical location to be absent.

## Durable publication

Setup uses an OS-held lifecycle fence outside the removable state root, followed by the state/project locks in their defined order. The same fence coordinates runtime startup and scoped removal. Lock-file existence alone grants nothing.

A normalized request fixes canonical cwd, scope/mode and request UUID. Pending intent records expected/desired references before bootstrapping isolated storage. Publish authorized binding/references/exclusion updates and save the original successful result before clearing intent. Recovery recognizes already-published bytes, restores durability and continues that same intent. Conflicting foreign changes are preserved and reported.

Metadata uses bounded exclusive staging and fsynced file/directory operations. Partial staging is not committed state. Missing retained databases are never silently recreated as empty history. Existing corruption or unsupported formats refuse ordinary open/install before write pragmas.

## Scope and preservation

New directory scope leaves inherited references untouched. Repository scope publishes the common reference before removing only the effective directory override. Old UUID history and unrelated overrides remain.

Git exclusions are merged into the repository's local info/exclude with foreign bytes, modes and newline style preserved. Runtime setup does not rewrite tracked `.gitignore`. An explicit fresh-project request creates a fresh identity; it is not automatic history conversion. See [installation](../guides/installation.md).

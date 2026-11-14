# ADR 0002: Separate project identity from paths and use SQLite

## Status

Accepted and implemented, with recorded verification qualifications.

This record retains the decision's context, alternatives and consequences. Read it with the [current contracts](../README.md) and [verification scope](../guides/verification.md). Historical execution records are preserved separately; they do not certify arbitrary newer images or authorize installation, state replacement or publication.

## Context

The user wants isolated project conversations while storing databases under `~/.ac/{project_id}`. Worktrees may share a conversation space, but directory isolation must also be selectable. A full-path hash would change identity on relocation. Persistence does not need to match bd's Dolt backend.

## Decision

Generate UUID v4 project identities. Store one SQLite database under each project UUID. Use a Git common-directory reference for repository scope and a local `.ac/project.json` reference for directory scope. Default to shared repository scope inside Git and directory scope elsewhere. A directory reference takes precedence within its subtree.

Keep references Git-local and databases outside the working tree. Validate canonical root bindings, detect copied references, and require explicit relocation or new-project initialization when identity would otherwise be ambiguous.

Keep a versioned authorization record in each UUID's data-directory project.json. Encode canonical native path bytes without deriving identity from their display. Initialization uses ac-wide then project OS locks, a durable pending intent and retained successful request receipts. New directory scope applies at the current directory; repository replacement publishes its common reference before removing only the effective masking directory override. Preserve every old database. Replaying an older successful initialization must not restore its reference over a later replacement.

Use bundled SQLite with WAL, full synchronous commits, foreign keys and a version containing the WAL-reset fix. Back up before schema upgrades and refuse newer unsupported schemas without modification.

The user's 2026-10-15 implementation instruction selects SeaORM as the only query entry point. [ADR 0007](0007-seaorm-only-persistence.md) records that accepted revision; the SQLite integrity and transactional requirements remain in force.

## Consequences

Worktrees share by construction, fresh clones start independently, and path relocation need not lose history. Logical isolation is project-local even though physical storage is under the user's home directory.

UUID references require discovery and binding validation. A path hash is still suitable for short runtime names but not persistent identity. SQLite provides embedded transactional storage without introducing database-server lifecycle or replication machinery not required by v1.

Reference publication spans files and database commits, so a recovery journal is required rather than a multi-file atomicity claim. Discovery stays read-only and does not create a database, runtime or participant. The shared project operation invokes the core's default-channel bootstrap without manufacturing a host context. Host integration installation remains a separate, resumable part of the complete init workflow.

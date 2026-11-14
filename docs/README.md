# Documentation

Start with the [project README](../README.md) and [installation guide](guides/installation.md).

## Guides

| Guide | Purpose |
| --- | --- |
| [Installation](guides/installation.md) | Build, setup, managed launch, preserving updates and scoped removal |
| [CLI operations](guides/cli-operations.md) | Actor selection, commands, bounded pages and retries |
| [MCP operations](guides/mcp-operations.md) | Typed tools and managed connection authority |
| [Collaboration](guides/collaboration.md) | Shared task ownership, channels and reservations |
| [Troubleshooting](guides/troubleshooting.md) | Explicit refusals, Unknown results and recovery |
| [Runtime diagnostics](guides/runtime-diagnostics.md) | Safe bounded logs and health |
| [Development](guides/development.md) | Incremental builds, generated sources and focused checks |
| [Verification](guides/verification.md) | Required gates, evidence ownership and platform limits |
| [Native acceptance](guides/native-acceptance.md) | Isolated actual-host verification |
| [Workloads](guides/workload.md) | Synthetic functional, persistence and timing profiles |
| [SQLite probe](guides/sqlite-probe.md) | Disposable storage comparisons |
| [Compatibility](guides/compatibility.md) | Live capability checks and platform scope |

## Design

| Contract | Responsibility |
| --- | --- |
| [Architecture](design/README.md) | Product model and crate boundaries |
| [Public interfaces](design/interfaces.md) | Actors, shared operations and wire contracts |
| [Storage](design/storage.md) | Dispatch aggregates, atomic history and persistence |
| [Delivery](design/delivery.md) | Native claims, uncertainty and independent reads |
| [Runtime IPC](design/runtime-ipc.md) | Ownership, bounded routing and observer lifecycle |
| [Project initialization](design/project-initialization.md) | Discovery, identity and recoverable publication |
| [Read API](design/read-api.md) | Typed snapshots, windows and receiving scopes |
| [Viewer](design/viewer-contract.md) | React UI, generated SDK, replay and explicit user actions |
| [Backup](design/backup.md) | Internal consistent snapshots and safe publication |
| [Migrations](design/migrations.md) | Frozen model baseline and named migration history |

The [numbered ADRs](adr/README.md) retain decisions and rationale. Later explicit amendments supersede earlier behavior as indicated by the index and current contracts. Historical execution journals and raw local evidence are preserved separately; they are not build inputs except for the deliberately retained [generator provenance](../tools/provenance/chigiri/README.md).

# ADR 0007: Use SeaORM for all database queries

## Status

Accepted and implemented, with recorded verification qualifications.

This record retains the decision's context, alternatives and consequences. Read it with the [current contracts](../README.md) and [verification scope](../guides/verification.md). Historical execution records are preserved separately; they do not certify arbitrary newer images or authorize installation, state replacement or publication.

## Context

During implementation the user required SeaORM for SQLite persistence and prohibited queries outside that ORM. The earlier rusqlite dependency declaration had no persistence implementation behind it.

The user also required SeaORM 2.x migration conventions: derive the initial schema from models, then retain named migration history for later changes. A handwritten initial column list would not meet the model-derived requirement; synchronizing mutable models on every open would not preserve historical migration meaning.

## Decision

Replace the direct rusqlite dependency with SeaORM. Model relational state with SeaORM entities; compose queries and schema migrations with entity and SeaQuery APIs executed through SeaORM. Do not use handwritten SQL, raw SQL statements, custom SQL fragments, direct driver queries, or SQL migration files, including in tests.

Configure SQLite pragmas and immediate write transactions through SeaORM's connection and transaction options. Native online backup is a storage operation, not permission for an alternate query path. If a required operation lacks a suitable ORM interface, record the limitation and resolve it explicitly while preserving the accepted backup and integrity guarantees.

The current online backup uses a SeaORM read snapshot and a fresh canonical database initialized by the named migrator. Copy every retained table, framework history and autoincrement counter, validate the closed result and publish it without replacement. Compare bounded schema-catalog metadata first so unknown objects/columns cannot be silently omitted. This is a consistent logical backup, not a byte-for-byte native backup claim. See the [backup implementation](../design/backup.md).

Backup progress observations do not own copying or publication policy. The ordinary path and its observed variant share all snapshot, validation and create-new guards. A process interruption can retain private unpublished staging data; later attempts preserve it and never infer deletion authority from its filename. Complete public destinations remain protected against replacement.

Use SeaORM 2.x `MigrationTrait`, `DeriveMigrationName`, `SchemaManager` and an ordered `MigratorTrait` registry. Freeze the initial entity models once and generate their table definitions with the official `Schema::create_table_from_entity` API; supplement them with typed constraints and indexes. Apply later changes as additional named migrations. Retain standard `seaql_migrations` applied/pending tracking and the application's checksum ledger. AC's dependency declarations must not opt into `schema-sync`, and AC must not invoke model synchronization or automatically apply current model differences to a retained database. The selected official `sea-orm-migration` 2.0.4 crate transitively compiles `schema-sync` support through its own SeaORM dependency. This library capability does not change AC's schema path: existing databases change only through the ordered named migrations. The pinned source check rejects application opt-in, while frozen-model/history and retained-database tests verify the actual behavior.

The one-time snapshot helper is repository tooling, not an official SeaORM CLI generator. The ORM owns DDL generation and the migration framework owns application history. The [migration workflow](../design/migrations.md) explains this distinction and the immutable baseline.

The retained-history optimization follows the same rule: schema v3 adds two typed indexes in a new named migration, preserving the sources/checksums of v1 and v2. The ordinary guarded upgrade publishes a validated backup at the source version before applying pending history. Performance work must preserve the unpublished-event commit guard, bound-query semantics, partial uniqueness constraints and FULL synchronization. A measured lookup improvement does not establish a passing end-to-end workload; see the history-index evidence.

## Consequences

The ac-core storage port remains ORM-independent. Only ac-store depends on SeaORM. Async ORM work uses the driver's blocking workers; it must not block the Tokio reactor. Engineering checks enforce the database boundary and reject raw-query escape hatches. The actual linked SQLite still must be at least 3.51.3. ORM adoption does not establish this fact or relax transaction/crash tests.

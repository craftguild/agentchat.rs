# Model baseline and named migrations

All SQLite queries use SeaORM; schema changes use SeaQuery builders through SeaORM. Handwritten SQL and direct-driver queries are not allowed, including tests.

The initial schema is generated from the frozen initial entity models. Verify it with `python3 tools/generate-initial-migration.py --check`; never regenerate it from evolving current entities. Later schema steps use the SeaORM 2.x named `MigrationTrait` / `MigratorTrait` history with immutable released inputs. Automatic model synchronization into an existing database is not a migration.

The current breaking Dispatch design requires fresh named schema 14 / format 3. Released migrations 1 through 13 remain frozen. Ordinary open and direct migration refuse older project formats with explicit `agentchat install --new-project` guidance before writes. Old history is not converted and no real project is reset as a side effect of inspection or verification.

Initialization and upgrade boundaries retain the migration lock, exact expected schema/identity and validated internal backup. SQLite table reconstruction, where part of frozen history, uses a private schema-only connection with foreign-key enforcement restored through closing/reopening and explicit integrity/FK checks. Ordinary writers always enforce foreign keys and WAL/FULL.

New lifecycle changes must define current state, append-only transition history, initial facts and compare-and-set guards before implementation. Header/Detail relationships and history commit atomically. See [storage](storage.md) and [ADR 0032](../adr/0032-delivery-state-machine-and-ack-removal.md).

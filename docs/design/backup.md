# Internal consistent snapshots

Backup is an internal migration/recovery safeguard, not a public CLI or MCP backup command. Callers must supply the existing authorized project and owned destination intent; copying an arbitrary live SQLite file is not this contract.

Store reads a consistent SeaORM snapshot in bounded chunks and constructs the destination through entity models/named schema history. Validate project/schema, foreign keys, aggregate invariants, event order and complete counts/digests before publication. Source reads grant no mutation or native authority.

Publication uses a new owned destination, private staging and explicit file and directory synchronization. Existing files, symlinks, hard-link aliases, foreign metadata or changed intent are refused. A partial staging file is never promoted as a completed backup or overwritten to manufacture recovery success.

Pending intent fixes original identities and hashes. Recovery either confirms the already-published exact result or retains the conflict and original evidence. No backup failure resets a real project, converts an unsupported format or silently restores over live history. See [migrations](migrations.md) and [initialization](project-initialization.md).

# Runtime diagnostic logs

The runtime records a fixed diagnostic vocabulary without accepting message text, command arguments, paths, host session IDs, arbitrary error messages/details or credentials. Each JSON Lines record contains only version 1, the project UUID, the runtime instance UUID, diagnostic Unix wall time and a fixed event with an optional stable application error code. Wall time can move backwards; only the SQLite event sequence defines application ordering.

## Location and bounds

Logs live at `AGENTCHAT_HOME/<project-uuid>/logs`, using the selected agentchat state root (`$HOME/.agentchat` by default). `logs` requires effective-user ownership and mode `0700`. Current/archived files and the empty `writer.lock` require regular single-link files owned by that user with mode `0600`. Unexpected permissions, symbolic links, hard-link aliases and foreign filesystem types are refused, not repaired.

| Resource | Bound |
| --- | --- |
| Producer queue | 128 fixed-shape records |
| Serialized record, including newline | 512 bytes |
| Current file | `runtime.jsonl`, at most 1 MiB |
| Archives | `runtime.jsonl.1` and `runtime.jsonl.2`, at most 1 MiB each |
| Retained diagnostic bytes | At most 3 MiB, plus an empty writer lock |
| Predecessor writer-lock acquisition | 500 ms |

The sink uses a nonblocking bounded send. Saturation drops the diagnostic record and increments a saturating current-instance counter. A blocking worker owns filesystem I/O independently from the application coordinator and database transactions. It rotates before a complete record would exceed the current file limit: remove the oldest archive, move archive 1 to 2, move current to 1, create a new current file, and synchronize the directory. Known inode/length/modification metadata fences ordinary outside replacements or edits before appending/rotating. The private directory and OS writer lock coordinate agentchat; they are not compare-and-swap protection against arbitrary noncooperating same-user writers.

The log is best effort and cannot acknowledge delivery, mark a message read, grant a reservation, renew host presence or undo a committed operation. An application success remains successful if logging subsequently fails. The worker stops after a write/rotation failure; it does not start an unbounded retry loop. Retained sinks do not keep their owner worker alive. On ordinary close, stop accepting records, drain the bounded pre-close queue and synchronize output; records racing with close may drop.

Writers and offline readers explicitly unlock before reporting successful completion, with a drop guard for error/cancellation paths. [Linux flock semantics](https://man7.org/linux/man-pages/man2/flock.2.html) associate the lock with the open file description: closing a parent's descriptor alone need not release it while an inherited duplicate survives in a child. Explicit unlock avoids extending a completed operation's lock lifetime through that temporary inheritance.

## Health and inspection

`agentchat status --json` reports `result.runtime.diagnostics`, including `state`, `dropped_records` and the queue/file budgets. States are `running`, `closing`, `closed` and `failed`. The counter counts producer rejections (full/closing/closed sinks); an I/O failure or crash may additionally lose already-accepted queued/tail records without exact loss accounting. These counters/health belong to this runtime incarnation and reset on a new runtime. Routine events include coordinator readiness/shutdown, request success/failure, control/maintenance failures, invalidated project references and rejected connection capacity. The logger never serializes the request or an error's text/details.

`agentchat doctor --json` reports `result.diagnostic_logs`. With a reachable runtime it uses the same live health observation. Otherwise it opens an existing writer lock read-only and attempts a shared lock before validating retained files. A writer still holding the exclusive lock produces `busy`; absent logs produce `absent`. Unowned/invalid metadata is reported explicitly, without creating files or fixing it. Offline valid files report retained bytes/count and incomplete-tail bytes. A live doctor RPC can itself generate an ordinary request-success diagnostic; the offline inspector creates no logger and makes no repair writes.

## Restart and recovery

Runtime startup validates all retained files before appending or repairing a tail. Complete records must use the bounded known format and current project UUID. After an interrupted append, an incomplete final current-file fragment shorter than one record may be removed; complete earlier records remain. Archive gaps caused by interrupted rotation are allowed. New instance UUIDs distinguish a restarted runtime in the retained files. A process exit does not imply a final `stopped` record.

Malformed complete records, an invalid archive, an oversized file or unsafe metadata stops startup without truncating a different file's incomplete tail. Later detected outside edits disable logging and appear in live health. Doctor can diagnose the offline files without repairing or launching a runtime. There is no public log-repair command. After the owned runtime has stopped, an administrator can preserve and move the entire diagnostic directory aside and allow ordinary startup to create fresh logs. Do not remove the runtime ownership lock or alter SQLite/project references as a diagnostic-log repair.

Diagnostic rotation may lose old routine observations; it never prunes messages, events, receipts, deliveries or successful retry records. SQLite remains the authoritative retained history. The logger does not claim power-loss durability, atomic multi-file archive rotation or complete crash-stage coverage.

# Persistence and state machines

## Files and SQLite

Canonical project references authorize one UUID namespace under the selected private state root. Copying a reference does not authorize a new location. Ordinary writers use one SeaORM connection, foreign keys, WAL, synchronous FULL and a bounded busy timeout. Reads use bounded consistent snapshots. Direct driver queries, handwritten SQL and automatic schema synchronization are prohibited.

Current storage requires fresh named schema 14 / DB format 3. Released migrations 1 through 13 and initial model-derived inputs stay frozen. Older projects are refused before write pragmas with explicit fresh-project guidance, without history conversion or automatic reset. See [migrations](migrations.md).

## Current relational inventory

The fresh schema has 22 business/control tables plus two migration-management tables, excluding SQLite internal tables:

| Table | Primary key | Responsibility |
| --- | --- | --- |
| `dispatches` | `id` | Shared immutable sending Header; unique creation sequence |
| `dispatch_messages` | `dispatch_id` | Conversation body/author and optional direct Message root |
| `dispatch_notices` | `dispatch_id` | Typed structured notification content |
| `deliveries` | `id` | One recipient obligation per Dispatch/recipient pair |
| `delivery_state_transitions` | `id` | Append-only state facts, unique delivery/ordinal |
| `delivery_timeout_notices` | `message_id` | One original Message to one timeout Notice coalescing identity |
| `events` | `ordinal` | Independent typed audit log; unique public sequence and event UUID |
| `actors` | `id` | Native participants and the project-common anonymous user |
| `channels` | `id` | Channel identity and creation occurrence |
| `channel_memberships` | `channel_id, actor_id` | Explicit current receiving scopes and their Join boundaries |
| `sessions` | `incarnation` | Retained connection/context authority |
| `launches` | `generation` | Immutable managed native launch ownership |
| `message_streams` | `generation` | Exact native FIFO head and settled cursor per launch |
| `wake_batches` | `id` | Bounded wake scheduling groups for original deliveries |
| `mentions` | `message_id, recipient_id` | Frozen Message mention resolution |
| `read_receipts` | `message_id, actor_id, kind` | Independent automatic/explicit first-read facts |
| `reservations` | `id` | Cooperative reservation groups and owner/lifetime facts |
| `reservation_patterns` | `reservation_id, ordinal` | Ordered patterns belonging to each reservation Header |
| `idempotency` | `request_id` | Successful normalized operation identities and results |
| `project_meta` | `singleton` | Project identity, schema/format and shared causal allocator |
| `project_default_channels` | `position` | Ordered configured project defaults |
| `project_receipt_settings` | `singleton` | Automatic-read policy and processing timeout setting |
| `schema_migrations` | `version` | Frozen schema-history checksum records |
| `seaql_migrations` | `version` | Named SeaORM migrator bookkeeping |

## Dispatch Header and recipient Details

`dispatches` owns immutable UUID, Message/Notice kind, committed creation time/order and applicable channel scope. Exactly one `dispatch_messages` or `dispatch_notices` subtype must match its kind. A Message's required `dispatch_id` PK/FK is also its public Message UUID; body/author/direct-root relationships live in the Message subtype. A Notice has a typed kind and validated structured payload.

Each Delivery Detail has one required Dispatch owner and a unique recipient per Header. Zero recipients are valid. Reply roots are Message Dispatches in the same channel. Message reads, mentions and automatic/explicit reads select only Messages; mixed native Inbox/FIFO also includes Notices.

Allocate Header, payload, frozen recipients, initial histories, applicable audit facts and successful retry result in the same FULL boundary before external I/O. Reject wrong/missing/both subtypes, orphan/duplicate recipients and partial publication even when an earlier application error was ignored.

## Current state and append-only history

Core owns `Pending`, `Processing`, `Delivered`, `FailedTimeout`, `FailedLeft` and `DeliveredLate`. Every Delivery has immutable initial/consecutive transition rows: stable UUID, required delivery FK, unique positive ordinal, nullable initial `from_state`, typed `to_state`, UTC `changed_at`, and a captured `deadline_at` only on entry to Processing. Ordinal 1 records NULL -> Pending. Consecutive ordinals order tied timestamps; times cannot move backwards.

Store validates the persisted chain and first-fact projection, compare-and-sets private `state_version`, and appends history with current state in one transaction. `queued_at`, processing start/deadline, timeout, delivery and departure times are read projections of those first facts. Existing facts and routing identity cannot change; attempts cannot decrease. No-op saves, request replay and duplicate callbacks append no history. Bulk scoped departure uses the same validated CAS boundary. There is no history update/delete port or competing receipt store.

Only a first Processing claim starts its deadline. Unknown original input remains fenced after timeout. Complete original acceptance settles the exact native head and delivery history in the same FULL commit, including any frozen automatic read. There is no ACK state, field, operation or manual wait. Explicit Message reads remain independent. See [delivery](delivery.md) for legal transitions.

## Events, order and transaction proofs

Events are separate immutable typed occurrences, not Dispatch parents or native work. Explicit notification policy creates Notice Dispatches. Audit replay alone creates no recipient obligations. A transactional shared sequence allocator orders Events and Dispatches causally; audit and receiving/Inbox heads remain distinct. A private consecutive audit ordinal checks full replay despite legitimate gaps in the shared public sequence.

Already-ready groups are bounded to eight writes. Each independent semantic failure rolls back its savepoint; infrastructure or outer failure exposes no provisional results. A transaction-local proof can reuse a successful complete Dispatch check across savepoints, tracking all affected Header IDs monotonically and falling back after its fixed bound. Every outer/root commit still performs the complete Dispatch and unpublished-event checks. The proof is not an authority cache and never crosses transactions/connections.

Native selection may try a 64-Header prefix using the same scope/predicate/decode. A miss executes the complete selector at the original cursor; it does not advance state or drop work. No new persistent lifecycle, index or queue is introduced by these optimizations. [Internal snapshots](backup.md) and [initialization recovery](project-initialization.md) retain their own ownership and exclusive publication fences.

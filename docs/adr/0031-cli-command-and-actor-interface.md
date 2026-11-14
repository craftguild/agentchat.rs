# ADR 0031: CLI commands, shared APIs and explicit operation actors

## Status

Accepted and implemented, with recorded verification qualifications. Later scope is amended or superseded by [ADR 0032](0032-delivery-state-machine-and-ack-removal.md).

This record retains the decision's context, alternatives and consequences. Read it with the [current contracts](../README.md) and [verification scope](../guides/verification.md). Historical execution records are preserved separately; they do not certify arbitrary newer images or authorize installation, state replacement or publication.

## Context

Pagination is currently exposed globally even though only nine finite commands accept it. The opaque `--context` name obscures the distinction between a native conversation, an AgentChat participant and a runtime connection. Default finite output is currently pretty-printed JSON rather than a human presentation. `status` includes the information returned by `whoami`, and the CLI has public watch and backup surfaces that the user wants to remove across shared APIs.

The desired actor selector names the native host and its chat ID directly, or selects one project-common anonymous user. This requires a shared actor model, not transport-specific impersonation or a fictitious native host for humans.

## Decision

### 1. Common options and project routing

The following table compares the source inspected for this publication with the accepted target. It is a specification, not current executable help.

| Current common option | Target common option | Value, omission and meaning |
| --- | --- | --- |
| `--json` | `--json` | Boolean flag, off by default; select machine JSON instead of plain human output |
| `--project PATH` | `--project PATH` | Optional directory; omission uses cwd as the discovery starting directory |
| `--context UUID` / `AGENTCHAT_CONTEXT` | Removed from public CLI selection | No context-derived project fallback; private integration descriptors remain internal |
| None | `--by-host HOST` | Exact lowercase Enum `codex` or `claude`; requires `--by-agent-id` |
| None | `--by-agent-id CHAT_ID` | Opaque native chat ID string; requires `--by-host`; not the internal participant UUID |
| None | `--by-user` | Boolean flag selecting the project-common anonymous user; conflicts with both agent selector options |
| `--request-id UUID` | `--request-id UUID` | Optional successful-mutation retry key, generated when omitted; existing applicability restrictions remain |
| `--limit N` | Command-local only | Finite page size, default 50, valid range 1 through 200 |
| `--after CURSOR` | Command-local only | Nonnegative decimal cursor, default 0 |
| `-h`, `--help` | Unchanged | Offline help at each command level |
| `-V`, `--version` | Unchanged | Offline root version display |

Resolve `--project` or cwd through the existing repository/directory namespace rules. It is a directory, not a project UUID or an implicit actor selector. Missing cwd requires an explicit path; routing must not silently borrow another actor's directory. Keep existing scoped/global uninstall routing semantics.

Actor syntax is exactly one of:

```text
--by-host codex --by-agent-id CHAT_ID
--by-host claude --by-agent-id CHAT_ID
--by-user
```

Reject partial pairs, conflicting selectors and unsupported host values, including `Codex` and `cLaude`; do not silently lowercase them. Treat chat IDs as case-sensitive opaque strings using the existing nonempty, 1,024-byte bound, without assuming a UUID format. Do not introduce `--agent-id`, `--user` or `--context` aliases. Public CLI selection does not fall back to the old context environment variable.

Globally visible actor flags select a subject; they do not guarantee that every operation is meaningful or permitted for that subject. Required actor operations reject omission rather than defaulting to a user or guessing from cwd. Observer reads may remain actor-free; when an actor is supplied, apply its ordinary membership/read scope. Actor-independent launch, installation, MCP-server and Viewer entrypoints reject supplied actor selectors as inapplicable rather than silently discarding them. Offline help/version retains its early-exit behavior.

### 2. Command-local grammar

`PAGE` below means local `--limit N` and `--after CURSOR`. These appear only on the exact leaf command, not root/group help. For example, use `agentchat history --limit 20`, not `agentchat --limit 20 history`. `BODY` means mutually exclusive `--body TEXT` / `--stdin`, retaining implicit nonterminal stdin, literal text and the 65,536-byte UTF-8 bound.

The subsequent user-approved message amendment (2026-10-30) strips leading/trailing Unicode White_Space from new Post/Reply bodies in the shared application action, for either actor kind and every transport. Internal text is preserved. Mention resolution, stored body and event preview/byte counts use the stripped text. The input bound and request identity still use submitted arguments; existing messages and successful receipts are not rewritten. Native transport continues to preserve the complete stored body, including historical whitespace.

Actor values: **required** means explicit selection, **optional** means an observer read can omit it, and **independent** means no operation actor is needed. An actor-independent command must not treat a supplied selector as permission to borrow a native connection or turn the Viewer into an actor.

| Current public leaf command | Target command | Target local arguments/options | Actor |
| --- | --- | --- | --- |
| `viewer` | `viewer` | None | Independent; context-free |
| `launch HOST` | Unchanged | `--resume SESSION_ID`, `--name HANDLE` | Independent; new verified native launch supplies its own actor |
| `mcp` | `mcp` | Remove `--restore-context UUID` | Independent; native routing supplies the verified agent |
| `install` | Unchanged | `--scope repository\|directory`, `--default-channels LIST`, `--relocate`, `--new-project`; ADR 0030 adds `--auto-ack true\|false`, `--auto-read true\|false`, `--delivery-confirmation-timeout DURATION` | Independent |
| `uninstall` | Unchanged | `--global` | Independent |
| `status` | Removed; consolidated into `whoami` | None | Required on the replacement |
| `doctor` | Unchanged | None | Optional diagnostic actor |
| `backup DESTINATION` | Removed | None | Not applicable |
| `join` | Unchanged | `--name HANDLE` for agent naming | Required |
| `leave` | Unchanged | None | Required |
| `whoami` | Consolidated `whoami` | None | Required |
| `agents list` | Unchanged | `PAGE` | Optional |
| `channel create NAME` | Unchanged | None | Required |
| `channel join NAME` | Unchanged | None | Required |
| `channel leave NAME` | Unchanged | None | Required |
| `channel list` | Unchanged | `--state active\|archived\|all` (default active), `PAGE` | Optional |
| `channel members NAME` | Unchanged | `PAGE` | Optional |
| `post` | Unchanged | `--channel NAME`, `BODY`; remove `--read-timeout` under ADR 0030 | Required |
| `reply ROOT_MESSAGE_ID` | Unchanged | Repeated `--read-message-id UUID`, `BODY`; remove `--read-timeout` under ADR 0030 | Required |
| `message MESSAGE_ID` | Unchanged | `PAGE`, paging the ADR 0030 receipts | Optional |
| `history` | Unchanged | `--channel NAME`, `--thread ROOT_ID`, `PAGE` | Optional |
| `search QUERY` | Unchanged | `--channel NAME`, `--thread ROOT_ID`, `PAGE`; case-sensitive literal search | Optional |
| `inbox` | Unchanged | `PAGE` | Required; receiving agent only |
| `delivery ack DELIVERY_ID...` | `ack DELIVERY_ID...` | Nonempty exact delivery IDs | Required; receiving agent only |
| `read MESSAGE_ID...` | Unchanged | Nonempty exact message IDs | Required |
| `events` | Unchanged | `--channel NAME`, `PAGE`; remove `--since` | Optional |
| `reserve PATTERN...` | Unchanged | `--ttl SECONDS` (default 600, range 30 through 3,600), `--reason TEXT` | Required |
| `reservations list` | Unchanged | `--owner HANDLE`, `--all`, `PAGE` | Optional |
| `reservations renew RESERVATION_ID` | Unchanged | Required `--generation N` | Required |
| `reservations release RESERVATION_ID` | Unchanged | Required `--generation N` | Required |
| `watch` | Removed | Remove `--since` / `--channel` follow entrypoint | Not applicable |

Preserve existing constraints on unchanged options, including init's mutually exclusive relocate/new-project meanings, immutable agent names, whole-set ID validation, exact reservation generations and offline help/version. Hidden version-coupled native/helper commands remain internal protocol entrypoints; this inventory does not make them public operations.

### 3. Native identity and first-class anonymous actors

Keep three different identities explicit:

| Identity | Issuer | Purpose |
| --- | --- | --- |
| Native chat ID | Codex or Claude | Opaque conversation ID supplied to `--by-agent-id` |
| Internal actor/agent UUID | AgentChat shared participation | Durable author, owner and receipt identity |
| Context ID and incarnation | AgentChat runtime after verified host facts | Exact connection/lifetime routing and fences |

`whoami` is read-only and issues none of these IDs. First Join creates the durable participant when absent; rejoining the same project/host/chat tuple retains its internal identity and immutable agent name. Two different hosts may use the same chat ID without colliding. Use the full tuple, not a project-wide chat-ID lookup.

The CLI pair selects an existing, exactly verified host association. Join obtains host/version/OS/lifetime facts from that association and the existing host adapter, not user-authored metadata fields. A selector alone creates no launch authority. Preserve original owner, project, current root, incarnation, generation, Leave, pause/approval and Unknown/FIFO checks. Missing or uncertain verified association returns the existing explicit session/integration error; never guess by name, directory, process ID alone, another host or an older generation.

Introduce typed shared actor kinds `agent` and `user`. The anonymous user is one durable project-common principal, not one per invocation or OS user. Create its participation through Join; a read-only lookup before first Join reports unjoined without writing a record. User Join/Leave, mandatory/default channel membership, posting, replies, explicit reads, channel creation and reservation ownership use the common application rules and successful-request retry semantics. The common anonymous label is `user`; agent naming with `--name` does not rename that user. The actor kind distinguishes it from an agent with the same display text. Supplying `join --name` for the anonymous user is an invalid argument, not a rename or an ignored option.

All local callers selecting this anonymous principal share its participation and ownership. Any such caller may renew/release its reservations with the correct generation. No individual human attribution or new authentication system is claimed. Do not infer native host presence for the user or require a fake Codex/Claude connection to perform its ordinary operations.

Selected-user content reads enforce the same membership rules as selected-agent reads. Lack of a native context must never downgrade a selected or unjoined user to the context-free all-history observer path.

Users have no native input stream, receiving queue, automatic ACK/read or wake. Exclude them from direct/group/passive recipient expansion before deliveries and confirmation targets are created. Membership does not imply receivability; a named nonreceiving target cannot produce a queued-delivery success. `ack` and `inbox` with a user selector return a clear shared nonreceiving-actor error, rather than accepting another actor's delivery or returning fabricated success. Manual read needs channel membership and valid message IDs, not an original delivery, so a user may create an explicit read with `delivery=null`.

Author, owner, reader and membership projections identify the typed actor. Do not encode a user as `HostKind::User` or overload an agent-only UUID field to pretend it is an agent. Agent-only listings and host statistics remain agent-only; channel membership and reservation/message detail can represent either kind. Existing `agents`/`joined` agent counters retain their agent-only meaning; whoami reports the selected user's participation separately. Retain bounded paging and typed CLI/MCP/Viewer reads. Evolve persistence with named SeaORM migrations, preserving existing agent UUIDs, authorship, receipts, reservations, retries and frozen initial schema history.

### 4. Consolidated shared APIs

These are breaking removals/consolidations, not hidden compatibility routes:

| Existing interface | Target |
| --- | --- |
| CLI `status`, MCP `agentchat_status`, separate public status query/result | Consolidated `whoami` / `agentchat_whoami` and one typed shared identity/health result |
| CLI `delivery ack`, MCP `agentchat_delivery_ack` | CLI `ack`, MCP `agentchat_ack`, one shared ACK operation |
| CLI `backup`, MCP `agentchat_backup`, public `Operation::Backup` and parameters | Removed |
| CLI `watch`, generic RPC `events.subscribe` and generic event follower | Removed |
| Finite `events` CLI/MCP | Retained, with `after` as the sole cursor input; remove MCP `since` too |
| `viewer.subscribe`, Viewer Query/SSE, `observer.attach`, native lifecycle/control streams | Retained for their distinct typed/read-only or host-lifetime responsibilities |

Consolidated `whoami` requires an actor and returns its identity/participation plus the old status information: project identity/head/defaults/settings, statistics, runtime/log health, capabilities, integration health and the selected agent's native progress/head when applicable. Agent-unjoined/user-unjoined is a read result, not an implicit Join. Native fields are absent/null for a user; do not report invented host disconnection. Include ADR 0030 effective receipt settings here and in install results.

MCP keeps its verified native agent routing and does not add arbitrary `by_user` or actor-switching fields. Remove public CLI restoration, but retain private descriptor/attachment machinery required for exact native recovery. Update shared bootstrap/tool routing for the consolidated read without giving an unbound caller borrowed native authority. Actor-free Doctor and necessary internal project-health composition remain available; they do not retain a public `status` alias or turn health queries into participation.

Remove public backup dispatch and its dedicated schemas/help/tests, while retaining internal migration, uninstall and configuration-recovery safeguards that happen to create backups. Preserve historical backup results/evidence. Removing generic event follow does not remove finite audit retrieval, Viewer change replay/backpressure or native message delivery. Move reusable terminal escaping out of a deleted watch module rather than losing that protection.

### 5. Default plain human presentation

Without `--json`, finite CLI results use plain text: labeled identity/health values, readable list rows, IDs needed for subsequent commands, continuation cursors and full message bodies. Do not pretty-print the JSON envelope or fall back to JSON for a result variant. Decoration, color, terminal-width formatting and a new banner design are not required. Keep the existing offline help/version behavior from [ADR 0010](0010-clap-and-cli-banner.md).

`--json` emits the machine envelope and typed data, including explicit failures. Normal successful human output goes to stdout and human errors to stderr; preserve documented exit categories, literal Unicode/multiline bodies and safe escaping of hostile terminal controls. Argument failures honor JSON selection. MCP stdio and native/helper protocol output remain protocol JSON; no human text may corrupt them. Preserve launch's controlling-terminal routing for JSON mode and Viewer's ready-URL result; native TUI/browser content is not a finite CLI result to reformat.

### 6. Composition, skill and migration boundaries

ADR 0030 continues to own automatic receipt policy, optional explicit reading, nullable read-with-reply, unified receipts and absence of positive notifications. Its public status display moves to `whoami`, and its identity labels accommodate typed user explicit readers. Automatic receipt creation still requires actual native acceptance and therefore cannot manufacture a user's automatic reads.

User-authored posts/replies monitor actual receiving agents under the same project deadline. If a deadline expires, retain the one bounded timeout audit record and queryable confirmation state, but create no native notification to the nonreceiving author or passive peers. Preserve author-Leave cancellation, late acceptance history and nonrecursive notification semantics. No hidden user inbox, polling loop, host start or alternative notification service is added.

Update the bundled skill and tool descriptions during product implementation: CLI actor arguments use exact host/chat IDs; MCP uses its verified attachment; `whoami`, `ack`, finite events and command-local paging replace obsolete names. Use effective ADR 0030 metadata rather than routine receipt calls. Document missing/retired connection recovery without prompting a new Join on every read. Developer-installed skills and actual host integrations are operationally separate.



## Verification and consequences

R36/T34 in [acceptance](../guides/verification.md) maps the grammar, shared actors, consolidated APIs, presentation, storage, skill and Viewer regression gates. Verify lowercase Enum parsing, partial/conflicting selectors, same chat ID on different hosts, wrong-project/stale-owner refusal, no read-time registration, anonymous participation/ownership/explicit reads and zero user deliveries. Test old-name rejection, nine local pagination surfaces, CLI/MCP parity, human/JSON errors/results and complete bounded bodies. Verify Viewer live replay and read-only behavior after deleting generic follow; preserve all existing Unknown/FIFO, platform, durability/resource and release requirements.

The result is an explicit actor interface and a smaller public command/API set, with useful default human output. Costs include breaking scripts/tool consumers, a shared principal migration and a deliberately shared anonymous ownership scope. Publication validation checks documents only; product tests, native behavior, generated schema verification and installation remain unexecuted for this design.

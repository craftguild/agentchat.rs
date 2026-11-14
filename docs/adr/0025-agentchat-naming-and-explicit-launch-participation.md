# ADR 0025: AgentChat naming, explicit participation and compact input framing

## Status

Accepted and implemented, with recorded verification qualifications. Later scope is amended or superseded by [ADR 0027](0027-process-continuity-across-executable-updates.md), [ADR 0028](0028-managed-auto-join-and-conversation-following.md), [ADR 0029](0029-autonomous-collaboration-and-channel-scopes.md).

This record retains the decision's context, alternatives and consequences. Read it with the [current contracts](../README.md) and [verification scope](../guides/verification.md). Historical execution records are preserved separately; they do not certify arbitrary newer images or authorize installation, state replacement or publication.

## Context

On this Mac, `ac` resolves to the existing operating-system command `/usr/sbin/ac`. The user wants the executable and related product names to use `agentchat`, without maintaining legacy aliases or migrating existing data.

Practical use also separates two intentions: starting a coding-agent terminal and participating in AgentChat. Automatic participation makes every managed terminal a delivery target before the user asks it to join. The user instead selects explicit participation for both Codex and Claude, while retaining a thin launcher to establish the real native CLI's process lifetime.

The Codex PID investigation observes exact thread IDs but no caller CLI PID in inspected shared-server MCP calls. The MCP subprocess parent is the shared server. Neither that parent nor a surviving server-side thread proves that the intended terminal still exists. An ordinary model-issued Join cannot manufacture that missing physical proof.

The user also wants a clearer agent-facing separation between the input header, JSON metadata, original body and footer. The current long header repeats the ACK/read/task distinction on every message. A compact, consistently separated frame can make attribution easier to locate without moving inline mentions out of their original body or introducing another body-fetch tool call.

## Decision

### 1. Use the full product name and start a new namespace

Use `AgentChat` as the product name and the following machine names:

| Surface | New name |
| --- | --- |
| Executable and CLI root | `agentchat` |
| Cargo packages and crate directories | `agentchat-*`, including `agentchat-cli` and the future `agentchat-web` |
| Rust crate references | `agentchat_*` |
| Product environment variables | `AGENTCHAT_*`, including internal launch/context/configuration variables |
| Default state home | `~/.agentchat`; override with `AGENTCHAT_HOME` |
| Directory-scoped project reference | `<directory>/.agentchat/project.json` |
| Repository-scoped project reference | `<git-common-dir>/agentchat/project.json` |
| Project database | `AGENTCHAT_HOME/{project_id}/agentchat.sqlite3` |
| Isolated host configuration root | `AGENTCHAT_USER_CONFIG_ROOT` |
| Installed MCP server and skill | `agentchat` |
| MCP tools | `agentchat_*`, for example `agentchat_join` and `agentchat_post` |
| Native MCP capability namespace | `agentchat/native_routing` |

Apply the same name to current internal product identifiers, hook/module templates, markers, temporary socket prefixes, diagnostics, build/install/CI rules and current documentation. Preserve bounded Unix socket path lengths, private permissions and actual process/image checks when prefixes grow. `make` and `make build` build the renamed executable; `make install` only copies that prebuilt artifact, by default to `/usr/local/bin/agentchat`.

Do not install an `ac` executable alias, retain old tool/skill aliases, read `AC_*` as fallbacks, discover old `ac`/`.ac` references or automatically reuse the old state namespace. Leave old databases, references, binaries and host configuration intact. Provide deliberate old-integration retirement and fresh `agentchat init` instructions; initialization must not silently remove an old MCP entry, hook or skill. Tests must not perform that retirement in real profiles.

A natural-language request to "join ac" may still mean AgentChat. The renamed skill explains that meaning and uses the new tools; this is not an old machine-interface compatibility path.

Retain the responsibility-based crate graph and ORM boundaries. A rename is not permission to regenerate the frozen initial schema, change released migration names/checksums or invent a replacement migration history. Adjust current provenance path keys for moved sources while preserving frozen contents and digests. Do not rewrite historical evidence manifests.

### 2. Separate launch authorization from participation

`agentchat init` initializes the project and installs its integrations. It does not start a native conversation or create participant membership.

`agentchat launch codex` and `agentchat launch claude` start the selected native CLI and establish a live authorization record for its exact conversation and project. Capture the actual native executable's PID, kernel process-start and boot identity, user/image provenance and original supervisor. A wall-clock timestamp or a Node/shell launcher's PID alone is insufficient. Never infer the conversation from the directory, display name or process ancestry.

The thin launcher does not proxy native chat/server traffic. Readiness and integration health establish eligibility to request participation, not participation itself. A successfully prepared launch starts unjoined: no agent membership, channel membership, delivery stream or reservation is created merely by launching it. Keep native trust/authentication/sandbox/approval behavior.

### 3. Explicit Join is the only activation point

| Origin and state | Join behavior |
| --- | --- |
| Live, verified launch of this exact conversation; never joined | Activate participation at a new committed boundary |
| Same authorized conversation already participating | Return its current participation; do not create another generation |
| Same original live CLI after explicit leave | Activate a fresh participation generation at a new committed boundary |
| Another Codex frontend on that same authorized UUID, original owner current | Apply the same existing/never-joined/left Join rules; create no second owner or participant |
| Ordinary launch without verified managed authorization | Refuse and explain the required managed launch command |
| Original owner/supervisor positively lost, or foreign/stale scope | Refuse; cached thread/MCP state supplies no replacement authority |

For Codex the request is authorized at conversation scope, not caller-terminal scope. Resolve the host-issued exact UUID to the original supervisor-published association and recheck its original physical process. Same-UUID A/B frontends share one participant; either may request rejoin after Leave while that original owner remains current. Positive original-owner loss refuses even when the other frontend survives. PID reuse cannot satisfy the original kernel identity, and unknown observation supplies no positive liveness. An unrelated fresh root has no managed authorization. Startup remains unjoined and recovery never autojoins.

The user requests participation in the agent prompt; the agent invokes `agentchat_join`. CLI and MCP use the same application operation. Shared policy checks explicit participation and generation transitions; host adapters supply the exact native identity and original-process proof under ADR 0022.

Join uses the trusted launcher authorization and its internal activation path. It must not authorize itself with a model-supplied PID, launch token, copied context, directory or arbitrary thread ID. Separate the launch-readiness handshake from the membership commit instead of removing the readiness check or joining early to release the first prompt. Only successful explicit activation creates membership and makes this conversation a delivery target.

For an unmanaged Join, return the ordinary version-1 error envelope with `code=session_required`, `retryable=false` and `details.reason=managed_launch_required`. Include a `details.required_command` for the identified host and an English message explaining that the conversation must be started with `agentchat launch codex` or `agentchat launch claude`. Reject without creating participants, delivery work or reservations. The skill reports this guidance to the user rather than automatically starting another terminal, retrying the same refusal or manufacturing a descriptor.

Natural-language intent is interpreted by the coding agent. It does not require an exact phrase parser in the transport. Preserve existing immutable handle and mutation idempotency rules; replaying an old successful Join must not reopen a retired participation generation.

### 4. Retain lifetime, ordering and fresh-only re-entry

The live launcher authorization and an active participation generation have different lifetimes. Leave ends the latter and cancels future recipient work, but does not terminate the CLI or discard its independently live authorization. A subsequent explicit Join may use that same original CLI with a fresh participation generation and context binding. Do not revive a retired generation or let an old callback replace the new one.

Do not deliver messages from before Join or from a leave/offline interval. Shared channel history remains retained for explicit reads and the future viewer. Rejoin does not replay or reinterpret an earlier unknown native submission, clear its historical uncertainty or recall input already accepted by a host. Complete body acceptance, automatic delivery ACK, explicit read and task acceptance retain their separate ADRs 0018/0019 meanings.

Runtime/MCP recovery may restore an already-active generation under the same original live authorization. It must not join a never-joined or explicitly left conversation. A managed exact-ID resume after CLI exit also starts unjoined; it requires a new explicit Join. Positive original owner/supervisor loss stops new native submissions and retires active recipient work. Observation errors suspend delivery without inventing process death; surviving servers/helpers or other terminals cannot extend the original authority.

Keep the launch-selected conversation fixed. In-process conversation switching does not transfer authorization. Exit and managed-launch the selected conversation to participate elsewhere. Codex Esc alone remains an interruption of native work, not an AgentChat leave or extra application pause.

### 5. Validate minimal Codex startup before product changes

Set the child process working directory to the selected project directory and test this candidate using the matching actual native executable:

```text
/path/to/native/codex resume EXACT_PRECREATED_THREAD_ID
```

The candidate omits `--cd` and `--remote`. Independently verify the actual shared daemon, selected thread, working directory, native defaults and kernel owner. The earlier PID probe demonstrates selected plain-resume association on Mac Codex 0.163.0; it does not validate this revised product flow or picker filtering. Compare the real `/resume` picker in two directories under ordinary and managed startup without selecting another conversation or invoking a model unnecessarily. The reported directory-filtering cause remains unresolved. Do not assert that removing the flags fixes it, or restore a proxy/configuration override to force the expected result. If the minimal route cannot satisfy the contract, report the concrete observations before adopting another launch policy.

Two Codex frontends can display the same thread and the observed MCP metadata identifies that thread rather than the caller CLI. The user now explicitly accepts that conversation-level boundary. Verify same-root Join before first activation, duplicate Join and rejoin after Leave against the one original live owner; after its positive loss the other frontend must be refused. Do not transfer ownership to a surviving frontend or claim caller-terminal attribution. The accepted decision establishes feasibility of this admission policy, not production behavior, recovery or complete native validation.

### 6. Compact agent-readable input

Use this exact message-frame structure for both native adapters:

```text
[AgentChat:Message]
--
<METADATA_JSON>
--
<BODY_UTF8>
--
[/AgentChat:Message]
```

The angle-bracket names are grammar placeholders, not emitted text. `METADATA_JSON` is one compact canonical JSON line with the existing project, generation, delivery/event/message/thread/channel and author attribution, decimal-string sequence, `addressed` and `body_utf8_bytes`. Keep those fields and their types; omit the body from JSON and do not duplicate metadata in the header. `BODY_UTF8` is the complete original message body with all Markdown, mentions, leading/trailing whitespace, line endings and Unicode bytes unchanged. Do not pretty-print or JSON-escape the body, summarize it or wrap it in a code fence. Retain the existing message and native input-size budgets.

Framing uses LF: header, `\n--\n`, metadata, `\n--\n`, body, `\n--\n`, footer. Framing bytes are excluded from `body_utf8_bytes`. The separator lines improve readability; they are not escaped or forbidden within a message body. A decoder reads the fixed prefix and metadata line, validates the declared UTF-8 body byte count and terminal suffix, and requires the canonical roundtrip. It must not split the entire input on `--`, search for the first footer-like string, trim text or ask the coding agent to count bytes. Bodies containing separator/footer lookalikes remain ordinary content.

Bounded non-message notices keep a distinct type and their existing JSON:

```text
[AgentChat:Event]
--
<NOTICE_JSON>
--
[/AgentChat:Event]
```

`NOTICE_JSON` is the existing canonical `NoticeInput` projection, including its event and explicit `data_omitted` flag where applicable. There is no separate message body, synthetic author or empty-body block for an event. Preserve the 8-KiB control-context budget and the prohibition on substituting a notice for the full body of `message.created`.

The common typed formatter/decoder owns these frames. Host adapters consume them unchanged through the existing ordered input contracts. Keep the long delivery-ACK/explicit-read/task-acceptance explanation in the installed skill, not in every header. Neither a header nor a footer claims successful delivery, automatic ACK, reading or work acceptance: native input construction precedes the acceptance/ACK commit. The layout does not change recipients, inline mention resolution, FIFO, uncertainty, read or task-acceptance policy.

The new namespace needs no legacy frame parser or format negotiation. Historical native transcripts keep their original frames; do not rewrite them to resemble newly validated input. Shorter markers and clearer sections are a presentation decision, not evidence of improved inference latency or comprehension.

## Validation and implementation handoff

R28/T26 cover the complete rename and isolated fresh installation. R29/T27 cover both hosts' explicit participation, unmanaged refusal, same-CLI rejoin and original-owner lifetime. R30/T28 cover compact lossless input framing and skill guidance. Existing T04/T06–T10/T13/T15–T18/T20/T23 retain their identity, FIFO, recovery, persistence and adapter obligations.

Startup/authority, naming, participation and framing were verified in separately scoped stages. Original failed aggregates and external limits remain explicit; this does not claim one passing workspace run on every final source.

Use focused checks during development and one final affected engineering/platform wave; preserve Mac timing gates and Linux functional/durability/resource gates with measurement-only Linux timing. No completed native/model/workload case is replayed merely to relabel an old result.

Tests use temporary `AGENTCHAT_HOME` and `AGENTCHAT_USER_CONFIG_ROOT`, with no `HOME`/`CODEX_HOME` substitution or incidental real-profile changes. Test both fresh and exact-ID managed resume, never-joined silence, prompt-driven Join, duplicate Join, unmanaged refusals, Leave/rejoin with no inactive backlog, normal/abrupt process loss, PID reuse, uncertain observation, stale callbacks, runtime recovery and unchanged full-body/ACK/read/approval behavior. Frame tests cover both canonical codecs, maximum bodies, literal separators and footer lookalikes, whitespace/CRLF/Unicode, invalid lengths/metadata, bounded event notices and actual Codex/Claude input consumers. No model-issued ACK or body-fetch step is added by the new layout.

The original documentation-only publication changed design and task tracking. It installed no binary, altered no real host setting and established no new native or release pass. Subsequent implementation and qualified verification are recorded in the completion evidence above.

### Implementation boundary for refreshed native descriptors

Physical readiness retains the original full native/supervisor proof and a separate private activation socket. A proved project/host/conversation-UUID index selects that original authorization even after the native SDK issues a new unbound descriptor. Reading it creates no binding or membership. The original supervisor serializes explicit activation and retains its publication intent through uncertain results. A second launcher cannot replace a current or unconfirmed original association; positive original-owner/supervisor loss permits a fresh managed launch, never implicit transfer to an ordinary frontend.

Core atomically commits Join, its participation generation, initial stream and idempotency receipt. A cached Join receipt also checks the original process; it cannot bypass owner loss. Successful Join includes its committed context and generation. Native MCP may omit the context argument because verified routing supplies it; generic unbound callers still require an explicit descriptor. A Join can receive its committed result through a verified same-conversation, same-original-owner native context replacement; only that matching current slot may restore the result. A later root switch, explicit end or loss/closure of the physical gateway controller still cancels the preserved wait. Other calls retain retirement cancellation, and restoration never joins.

Native capability epoch 3 rejects cached helpers implementing automatic startup membership. This private capability change is independent of the unchanged public result-envelope version 1 and the Codex version-2 local route format. Final engineering/native evidence for this implemented route is recorded in the completion evidence and results ledger, with their exact source/artifact/assertion scope and retained original failures.

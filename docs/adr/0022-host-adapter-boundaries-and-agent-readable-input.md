# ADR 0022: Keep host mechanisms behind common delivery contracts

## Status

Accepted and implemented, with recorded verification qualifications.

This record retains the decision's context, alternatives and consequences. Read it with the [current contracts](../README.md) and [verification scope](../guides/verification.md). Historical execution records are preserved separately; they do not certify arbitrary newer images or authorize installation, state replacement or publication.

## Context and starting implementation

Channels, messages, mention resolution, explicit reads, reservations and ordered recipient work belong to AC rather than to a particular coding-agent product. Native products expose different input and lifecycle mechanisms. Sharing MCP support alone does not establish unsolicited input, passive context, exact root identity or complete-input acceptance.

The verified 640-input ADR 0021 starting point supplies useful boundaries:

- `ac-core::delivery::MessagePort` accepts attributed complete input and reports accepted, explicitly not accepted, awaiting a receipt, or an error/unknown result.
- `ac-core::ports::HostGate` supplies current host facts and launch-owner proof.
- Shared application operations own recipient selection, durable FIFO, retry identity, retirement and atomic native cursor/ACK transitions.
- `ac-hosts` implements native APIs, hooks, files, process observations and preserving installation; CLI and MCP adapt the same application operations.

That starting point has concrete host decisions in core code. Both `application/delivery/native.rs` and `application/delivery/stream.rs` select receipt recovery from `HostKind::Claude` or `HostKind::Codex`. `application/message_mentions.rs` and `application/delivery/policy.rs` contain Codex-specific eligibility checks. The identity enum, Store decoder and initial host-kind constraint also enumerate the two current hosts. The existing source check enforces crate dependencies, size and ORM use, but does not prohibit semantic host branches in core delivery policy.

That starting implementation was neither a dynamic plugin system nor fully extensible through ports alone. The implemented changes below remove concrete host decisions from shared policy; identity, registration and retained schema still require explicit changes for a new real host.

## Decision

### 1. Common policy and host-specific mechanisms

| Layer | Responsibility |
| --- | --- |
| `ac-core` | Project/channel/message rules, frozen attention targets, warnings, read deadlines, reservations, FIFO, bounded retry/uncertainty, retirement and atomic ACK/cursor policy |
| `ac-store` | SeaORM persistence and named history for those shared rules; no native host calls |
| `ac-hosts` | Native startup/attachment, exact root and owner observations, host controls, input encoding/transport, original receipt evidence, live API checks and preserving installation |
| `ac-runtime` | Bounded coordination and generic supervision, composing registered adapters with shared operations |
| CLI/MCP | Typed input/output adaptation and host selection at composition; no independent message, ACK or retry policy |

Adapters must not choose recipients by parsing body text, reorder or skip work, write business state directly, emit ACK/read transitions, renew authority from outbound success, or change host approval, trust, sandbox or interruption rules. Shared policy must not choose a native mechanism by concrete product name.

Composition may register and select a concrete adapter. Host labels remain valid identity, persistence and diagnostic data. A registration dispatch is distinct from a second implementation of business policy. Keep the seven-crate graph; runtime-loaded plugins and a new plugin framework are not required.

### 2. Typed facts and recovery capabilities

Each adapter supplies bounded, trusted facts through core-owned contracts:

- Exact native root, original managed-owner identity, lifecycle generation and current control state, including uncertainty. An unreadable observation is neither death nor permission to send.
- Supported complete addressed/passive input and the native conditions needed to offer it. Missing required mechanisms produce an explicit compatibility error; they cannot silently become previews, polling or forced model turns.
- The original-attempt recovery mechanism: receipt-only recovery where the host retains a correlated result, or conservative retention of an unknown result where it does not. These are mechanism capabilities, not permission to retry.
- Correlated whole-input outcome evidence for the exact committed head, token, owner, root and generation. Local publication is distinct from acceptance; errors, timeouts and cancellation are distinct from explicit refusal.

The application maps these facts to its one common state machine. Resolve adapter configuration outside write transactions; no native I/O or callback may occur while a database writer or installation lock is held. Capability data comes from the trusted adapter, never from model-supplied tool arguments. It establishes no current liveness, host permission or behavioral equivalence.

The initial durable claim must retain its original recovery meaning through restart and reconnect. An adapter/configuration change cannot reinterpret an unresolved head as a new submission or silently switch recovery strategy. If additional persistent fields are necessary, add a named migration and a conservative retained-state transition; never rewrite released migrations.

The current implementation retains the existing two head flags instead of adding a schema version: `awaiting_acceptance` encodes original-receipt recovery; `submission_pending` encodes retained uncertainty. Both false means a definitively settled refusal may use its remaining bounded retry budget; both true is invalid. Preparation rejects a declaration incompatible with an unresolved original head. A readonly receipt snapshot may precede a concurrent claim during native observation. Both selectors therefore repeat the pure mechanism comparison on the fresh head under the writer, using the already captured declaration; no native callback or configuration resolution is moved into the transaction. A local-publication result cannot change its mechanism, and Store rejects a direct transition from one unresolved mechanism to the other. Original positive outcomes still settle their exact attempt under the existing fences. Historical named migrations and original-schema backups remain unchanged.

Receipt-only polling emits no new input. Only positive acceptance or definite non-entry/refusal can settle an original unknown result under existing fences. The shared application commits verified acceptance and that delivery's ACK atomically. Explicit reading and work acceptance remain independent.

### 3. Adding a host

Implement native mechanisms in `ac-hosts` and wire them into the existing composition/installation path. Declare the consumed live API surface under ADR 0020. Adding a host must not require another recipient resolver, scheduler, ACK algorithm, read/deadline policy or host-specific branch in core policy. Explicit registration, typed identity/CLI changes and a new migration extending retained host representations can still be necessary. In particular, do not edit the frozen initial host constraint merely to admit a third kind.

The managed CLI lifetime contract in ADR 0018 remains required for a new native CLI integration. A remote service or headless agent with a different ownership model needs an explicit lifecycle decision; it cannot impersonate a local CLI. No additional real host is supported merely because a synthetic adapter passes.

### 4. Preserve inline context and expose derived attention

Inline mentions remain the authored source of attention targeting. For example, `@alpha` followed by an assignment and `@beta` followed by thanks retain their relationship to the respective body blocks. A top-level list alone cannot represent those relationships.

Core resolves existing CommonMark mentions once against the transaction's participant snapshot, retaining individual/group outcomes and warnings. Adapters consume the frozen result, including the recipient-relative `addressed` fact; they never reparse the body or guess which paragraph is a task for whom.

Attention targets are distinct from the delivery audience. Current joined participants still receive other participants' complete messages as addressed or passive input. A mention is not a private-message or access-control boundary. Existing wildcard, self-event, unknown/left-recipient, read-deadline and no-inactive-catch-up behavior is unchanged. No second authoritative `recipients` request field, union rule or automatic reply-to-author targeting is introduced.

Agent-readable input keeps a consistent common attribution frame: sender, channel, message/root identity, event order and whether attention selected this recipient, followed by the unchanged full body. This is the existing `MessageInput` direction, not a new public notification schema. Native wrappers may differ while preserving all meaning and original UTF-8 body bytes. Any later derived recipient listing must be labeled as the committed resolution, not another routing instruction or a newly inferred task assignment.

An addressed message may be a request, a question or an acknowledgement. Do not infer a mandatory reply, task acceptance or priority from `addressed=true`. Passive information and receipt events require no acceptance response. Full body context plus the shared skill explains when an explicit read, acceptance, inability or completion reply is appropriate. Transport bookkeeping stays in AC; normal reception requires no model ACK or extra inbox/body fetch.

A frame offered to a host must not claim that its own AC acceptance commit has already completed. Describe automatic ACK as AC's responsibility; report known receipt state only where independently established. Keep task content separate from authoritative routing/control metadata. Do not summarize, split, reorder or extract recipient-specific body fragments to make a notification shorter.

## Enforcement and acceptance

Map this decision to new R25/T23 and existing T06–T10/T13/T16/T17/T20/T22 in [acceptance](../guides/verification.md). Enforcement requires both automated checks and review of the semantic boundary:

1. Retain crate/ORM checks and add a pinned language-aware boundary check for concrete host branches in core recipient/delivery policy. Keep narrow explicit allowances for identity serialization, composition registration and immutable historical migration code; do not exempt all generated or test code. Cover literal and constant-reference patterns, including qualified names and imported/re-exported aliases, in `match`, `matches!` and `let` decisions. Keep diagnostic string bindings distinct from constant pattern references. Resolve constant references in their module/block and associated namespaces, including imports, re-exports, simple block tail expressions and default trait constants with explicit implementation overrides. A diagnostic constant must not taint another module's unrelated constant with the same final name. Respect separate type/value lookup, including type-only aliases. This remains bounded syntax analysis, not general Rust constant evaluation.
2. Run a shared real-Store contract suite against both recovery strategies and a third synthetic adapter implementation through the same ports. Adding that implementation must not modify shared scheduling or receipt algorithms.
3. Cover addressed/passive mixed FIFO, maximum unchanged bodies, positive acceptance with atomic ACK, explicit refusal, unknown outcomes, original receipt recovery, pause/approval, owner/root loss and stale callbacks. Include restart and strategy-change attempts against an unresolved original head.
4. Verify one body with separately mentioned recipient blocks reaches every selected participant unchanged, with correct per-recipient attention and no inferred block assignment, task acceptance or extra model bookkeeping.
5. Keep CLI/MCP parity and current native behavioral gates for affected changes. A fake adapter establishes the common contract only; actual host receipt, controls and lifecycle still require their own exact-environment evidence.

During development use focused affected checks. Group broad native/platform/ workload gates at final integration under ADR 0012. The completed R25/T23 evidence combines pinned syntax-aware enforcement, semantic review, real-Store contracts and affected native/platform/workload validation. Revalidate affected boundaries for later changes; this is not a proof against arbitrary source obfuscation or a behavioral certificate for an untested native host/version.

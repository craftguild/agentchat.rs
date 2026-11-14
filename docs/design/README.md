# Product and architecture

AgentChat connects separately running coding-agent conversations inside one project. A native agent has a stable project identity and a managed physical owner; neither a working directory nor a PID alone identifies a conversation. A project-common anonymous user supports explicit human actions without a native receiving queue. Context-free human observation remains separate.

## Project and collaboration model

Canonical repository or directory references select a UUID namespace. Linked Git worktrees share repository scope. Copying a reference does not grant a new location authority to open its history. Initialization, relocation and removal are explicit recoverable operations.

Participants join configured default channels automatically. Non-default channels require explicit membership. Only the first ordered default determines omitted-channel root posts; replies remain in their root's channel. Archive classification is a derived 30-day list projection, not a permission change. Agent reads, events and mentions retain membership scope. Human observers can inspect all project history. See [collaboration](../guides/collaboration.md).

Messages have immutable bodies and one optional direct root. Replies cannot form deeper trees. New bodies trim boundary Unicode White_Space before mention resolution and storage; interior bytes and raw-input retry identity remain. One source-aware Markdown interpretation supplies mention candidates and typed Viewer annotations. Authored image alt can contain candidates; code, destinations, titles, HTML attributes, generated text and unused footnotes cannot. An incomplete or failed parse refuses the whole mutation, including read-with-reply effects.

## Durable records and delivery

The immutable Dispatch Header has exactly one Message or Notice payload subtype and zero or more recipient Delivery Details. Each Detail owns an append-only initial/consecutive state history. Core owns lifecycle transitions; Store commits current state, history and the private compare-and-set version atomically. Events are independent typed occurrence records. Only explicit notification policy creates a Notice; replaying an audit event creates no native work.

Native FIFO uses committed Dispatch order. Complete original acceptance settles its exact head without a public ACK operation. Processing starts the captured deadline; waiting in Pending consumes none of it. Unknown input retains its original owner/root/generation/token and blocks uncertain successors. Automatic and explicit Message reads are independent first facts.

See [storage](storage.md), [delivery](delivery.md) and [ADR 0032](../adr/0032-delivery-state-machine-and-ack-removal.md).

## Runtime and host boundary

One owned project runtime serializes writes through a bounded coordinator. Already-ready operations may share a FULL transaction, with savepoints for independent semantic failures. Results and native effects never escape a failed or provisional commit. Native observation and host I/O run outside the writer. Bounded native jobs share the existing ingress; they do not add a collection timer, unbounded side queue or authority cache.

Codex uses a byte-transparent lifecycle-observing proxy. Claude uses native hooks/modules. Mechanisms report facts; they do not decide application deadlines, successors, resubmission or business notification policy. Same-project supported conversation switches follow only exact native evidence. Leave survives same-root recovery; a distinct verified root gets its own participation and no transferred backlog. Process continuity uses boot/PID/kernel-start/user independently of executable replacement. Startup still validates the actual executable and APIs.

## Crate boundaries

| Unit | Owns |
| --- | --- |
| `agentchat-core` | Domain policy, typed ports and shared application operations |
| `agentchat-store` | SeaORM SQLite implementation, schema and atomic persistence |
| `agentchat-protocol` | Typed CLI/MCP/runtime wire adaptation |
| `agentchat-hosts` | Native host mechanisms and exact process observations |
| `agentchat-runtime` | Project discovery, integration setup and owned runtime coordination |
| `agentchat-mcp` | Official-SDK stdio tool transport |
| `agentchat-web` | Fixed-project typed reader/writer ports and loopback HTTP/SSE |
| `agentchat-cli` | Executable grammar and transport composition |
| Generated viewer contract | Provenance-verified Chigiri types and mappings |

Neither CLI nor MCP owns business policy. Web has no direct storage or native authority dependency. All SQLite access uses SeaORM; schema construction uses SeaQuery builders through SeaORM. The static React frontend uses the generated operation catalog, not handwritten GraphQL in components.

## Operational boundaries

Reservations are cooperative logical path claims, not filesystem enforcement. No business operation launches arbitrary commands, changes host trust controls or treats activity/read receipts as task acceptance. Installation, authentication, real-profile cleanup and publication are distinct actions. See the [installation guide](../guides/installation.md) and [ADRs](../adr/README.md).

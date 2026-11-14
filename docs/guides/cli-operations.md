# CLI operations

Use `agentchat --help` and each leaf's `--help` for the complete grammar. Project discovery uses cwd or `--project PATH`. Human output is plain text; `--json` selects the versioned result. Mutation request IDs can be provided with `--request-id UUID` and must remain stable for successful/uncertain retries.

## Actors and common actions

Select an exact native pair with `--by-host codex|claude --by-agent-id CHAT_ID`, or the common anonymous principal with `--by-user`. Native IDs are opaque and case-sensitive. Mutations, whoami and Inbox require a selector. Observer queries may omit one; a supplied selector always retains its scope.

```sh
agentchat --by-user join
agentchat --by-user channel create planning
agentchat --by-user channel join planning
agentchat --by-user post --channel planning --body 'Please investigate this failure.'
agentchat history --channel planning --limit 20
agentchat channel members planning --limit 50
agentchat channel list --state all
```

Channel creation does not Join the creator. Configured defaults are mandatory memberships; other channels require explicit Join and can be left. Replies target one root UUID and stay in that channel. Root-post channel omission selects the primary default.

`post` and `reply` accept either `--body TEXT` or `--stdin`. Complete raw UTF-8 input is bounded to 65,536 bytes. `reply ROOT_UUID --read-message-id MESSAGE_UUID` adds only explicitly selected reads atomically with the reply. `read` preserves first times; observation alone records no read. There is no ACK operation.

## Finite reads and reservations

`agents list`, channel list/members, message, history, search, Inbox, events and reservations list use leaf-local `--limit` and `--after`. Default pages contain at most 50 items; limits range from 1 through 200. Events are passive finite reads, not a generic watch stream. Search is a case-sensitive literal substring.

```sh
agentchat --by-user reserve 'src/**/*.rs' --ttl 600 --reason 'Update the parser'
agentchat --by-user reservations list
```

Renew/release use the returned reservation UUID and exact generation. Acquire the complete logical pattern group atomically. Reservations coordinate intent without enforcing OS writes or proving task acceptance.

Setup, launch, MCP and viewer startup are actor-independent and reject selectors. `doctor` inspects without repairing. Public backup, status, watch, restoration and delivery-ACK aliases are absent. See [interfaces](../design/interfaces.md).

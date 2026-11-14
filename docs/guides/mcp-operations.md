# Typed MCP operations

`agentchat mcp` serves the official-SDK stdio tool transport. Ordinary users configure it through `agentchat install`; managed native routing establishes the exact connection. Starting a raw MCP process does not create arbitrary participant authority, and caller-supplied context IDs are not public actor selectors.

Tools invoke the same typed application operations as CLI commands. The live tool catalog provides bounded schemas for setup/inspection, participation, channels, messages, reads, events and reservations. There is no delivery ACK, public backup, generic event-follow or arbitrary-command tool.

## Identity and retry rules

- Managed startup participation is established outside the model after native readiness. `agentchat_whoami` inspects; it does not Join or repair authority.
- Explicit Join/rejoin must use the current original managed owner. Ordinary unmanaged conversations receive launch guidance instead of fabricated authority.
- Keep one successful mutation request UUID and its normalized arguments. A conflicting retry is refused. An uncertain result is recovered from retained facts, not replayed under another participant or new semantic request.
- Explicit reads name exactly the complete Message bodies being declared read. Omitted/null/empty reply read sets are independent of task acceptance.
- Native input contains complete original bodies. Observer reads, reposting, membership changes and receipts cannot resolve an unknown original native result.

## Limits and errors

Finite tool pages default to 50, maximum 200, with nonnegative decimal cursors. Complete message text is bounded to 65,536 raw UTF-8 bytes. Errors retain stable application codes and structured details; missing APIs, wrong roots and uncertain owners are not converted into empty success.

The setup tool retains its `agentchat_init` wire name while the CLI command is `install`. Current setup exposes `auto_read` and `delivery_confirmation_timeout_seconds`; removed ACK fields are rejected rather than silently ignored. See [interfaces](../design/interfaces.md) and the [collaboration guide](collaboration.md).

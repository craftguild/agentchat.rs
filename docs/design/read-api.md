# Typed shared reads

All transports use shared typed application reads. Store implements one bounded snapshot interface; presentation adapters do not issue their own database queries or obtain native authority. A read never mutates participation, delivery or first-read facts.

## Scope and consistency

Context-free observation sees project history. A selected native/user actor keeps its membership boundaries throughout resolution, paging, reconnect and error handling. No missing actor or failed selected query falls back to observer access. Inbox is a receiving projection for native actors, not a generic user mailbox.

Pages carry explicit cursor/checkpoint information. Validate the requested window against one captured snapshot; reject out-of-range and malformed bounds. At a stored exact head, an upper `before` bound is absent rather than one beyond the head. Below that head, an exclusive next-sequence bound is valid. Do not conflate an empty window with an unavailable or inconsistent source.

## Projections

- Message retrieval returns complete immutable Message bodies and bounded receipt pages. Conversation history/search/root/reply operations select Message Dispatches.
- Inbox includes both Message and typed Notice recipient obligations in shared Dispatch order. Audit-only Events do not supply native input.
- Actor directory retains left identities; joined roster and current channel membership are separately typed projections.
- Channel archive state uses captured server time and last root/reply activity, falling back to creation time. All defaults obey the same 30-day rule. Archived channels remain addressable, joinable and postable; posting revives the list state.
- Explicit read and automatic native-read facts remain independent of observer queries, search, visibility, replies without read IDs and task acceptance.

## Viewer replay

The fixed-project Viewer uses bounded snapshots plus one live committed stream. Apply events only once, retain the confirmed cursor, and recover gaps through the same typed query path. Capture the live source before catch-up to avoid losing the subscription/query boundary. Body/cache eviction must not remove membership or unread identity semantics. See [viewer](viewer-contract.md).

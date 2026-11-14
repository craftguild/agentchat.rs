# ADR 0016: Serialize direct full-body delivery to Codex

## Status

Accepted and implemented, with recorded verification qualifications. Later scope is amended or superseded by [ADR 0018](0018-launch-owned-ordered-full-body-delivery.md), [ADR 0022](0022-host-adapter-boundaries-and-agent-readable-input.md), [ADR 0032](0032-delivery-state-machine-and-ack-removal.md).

This record retains the decision's context, alternatives and consequences. Read it with the [current contracts](../README.md) and [verification scope](../guides/verification.md). Historical execution records are preserved separately; they do not certify arbitrary newer images or authorize installation, state replacement or publication.

## Context

The user selected direct message-body delivery rather than a notification preview followed by an MCP body fetch. Expected inter-agent traffic does not justify that extra tool/model round trip. Retrieved text would also consume model context. Mentions should control model activation, while other conversation remains available as passive context.

Native queue/add defers work to a later turn. Mixing it with direct history injection can expose a later passive message before an earlier queued message. The user instead selected turn/start for addressed messages and inject_items for other messages, with sequential delivery to each recipient.

## Decision

Use the shared app-server that owns the exact native Codex thread. For messages already selected for a participating recipient, deliver the complete body with author, channel, root/message identity and project sequence metadata:

| Recipient selection | Native operation |
| --- | --- |
| The recipient is in the message's resolved mention/wake set | `turn/start` with the message body as input |
| No mention resolves to that recipient | `thread/inject_items` with attributed message context |

Existing mention parsing, frozen recipient selection and own-echo suppression still apply. A mention of a different participant does not activate this one. Group mentions use the same resolved-recipient rule. Ordinary history access remains available, but normal reception requires no subsequent body-fetch call. Do not substitute a summary or truncate a valid body to a wake preview.

Maintain one ordered dispatcher per exact native recipient. Order committed messages by project event sequence, issue one native request at a time and wait for its result before sending the next message. Do not give the two APIs separate concurrent queues or fall back to queue/add, turn/steer or toolOutput as an unannounced alternate route. Uncertain/failed requests use the explicit recipient failure policy; acceptance of a later request must not hide an earlier failure.

Turn/start begins generation when idle and adds input to the current turn when active. Inject_items appends model-visible history without starting a user turn; its content can be used by a subsequent model request in that same active turn. This is not a promise to defer input until the original task is complete, or a promise that the model acts on messages in arrival order.

Preserve host model, sandbox, approval, collaboration and interruption controls. Do not issue turn/interrupt or override host settings to deliver a message. Do not advertise active input as universally nonpreemptive: selected commands finished normally, but all host control states still require validation.

Native submission success is not proof of model receipt, reading or an attached CLI. Do not refresh participation solely from successful outbound calls or loaded thread metadata. A PID-based association must separately prove the exact root, original process identity and exit behavior; a shared server PID is insufficient. The user's conditional acceptance of explicit rejoin after resume does not itself validate that mechanism or select a custom launcher.

## Supersession and implementation boundary

This supersedes ADR 0005's Codex steer-versus-start routing and the Codex default preview/body-fetch path. It does not admit a version, change Claude's native route, authorize automatic API resume of unavailable roots, or finalize the remaining participation/retirement and resume design. Earlier compatibility observations retain their exact historical scope.

No production adapter change follows merely from accepting this ADR. Keep the implementation paused for the requested independent host investigation. A custom launcher would be a separate interface decision under ADR 0005.

## Evidence and required checks

Disposable routing and retention evidence records selected Codex 0.163.0 cases on both operating systems. One additional Mac active-turn case submitted A through turn/start, waited for its response, then injected B. Persisted user items and the model's same-turn answer were A then B, and the original 15-second command completed once with exit zero. This supports the design; it is not a protocol-wide ordering guarantee.

T06 and T08 must cover the production dispatcher on each admitted profile: alternating addressed/passive messages, idle/active transitions, failure and restart boundaries, complete bodies and unchanged host settings. T04/T08/T16 must separately establish exact process/root association, normal and abrupt exit, resume and fork isolation. No presence or delivery guarantee may be inferred from the selected disposable cases.

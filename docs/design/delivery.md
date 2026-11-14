# Delivery and independent reads

## Lifecycle

Each recipient Detail belongs to one immutable Dispatch Header. Its canonical state is independent of model work, explicit reading and cooperative task ownership.

| State | Meaning |
| --- | --- |
| Pending | Queued; processing time has not started |
| Processing | First processing claim fixes the start and deadline before native I/O |
| Delivered | Complete original acceptance durably confirmed by the deadline |
| FailedTimeout | Deadline expired; the original uncertainty fence may remain |
| FailedLeft | Original receiving scope positively departed before completion/timeout |
| DeliveredLate | Original acceptance confirmed after expiry; both first facts remain |

Allowed changes are Pending to Processing or FailedLeft; Processing to Delivered, FailedTimeout, FailedLeft or DeliveredLate; and FailedTimeout to DeliveredLate. A no-op appends no transition. Terminal facts cannot be downgraded or resurrected. Late confirmation may atomically record both expiry and delivery when maintenance has not materialized expiry yet. See [storage](storage.md) for history fields.

The new-project processing timeout is 3,600 seconds, with supported positive whole-second configuration up to 2,592,000 seconds. Each first processing claim captures its own deadline. Later settings, retries or restarts cannot extend it. Posting time and Pending queue time do not consume it.

## Native claim and settlement

1. Validate the exact current context and commit Processing bookkeeping.
2. Observe native state or original receipts outside the database writer.
3. Recheck authority and the exact original selection under a fresh writer.
4. Commit the original head/token/attempt before native input begins.
5. Apply the correlated result through Core and return only after its FULL boundary.

Headers, recipient Details, generations, roots, tokens and native receipt meaning must agree. A successful host write, a sender post or a best-effort observation alone cannot establish complete acceptance. Actual positive completions retain their original application validation even if a reply receiver disappears.

Unknown input is never converted into a negative receipt to make progress. It keeps its original head and blocks resubmission or uncertain successors. Timeout alone does not release that fence. A later positive original receipt can settle late delivery; a definite non-entry follows the original retry policy. Native attempt and turn limits remain independent of processing timeout.

## Bounded coordination

Native jobs and completions are bounded. Up to eight already-ready writes may share one Immediate FULL transaction, with independent semantic rollback and all-or-nothing infrastructure failure. Query/control/lifecycle calls and same-stream dependencies remain FIFO fences. There is no batch-collection delay.

Empty preparation outcomes retain their exact original job's already-published cursor. After existing durable-head publication, a newer cursor permits one ordinary rescan. Consuming that transient marker creates no revision, so unchanged empty jobs cannot ping-pong. Stale or foreign empty callbacks cannot rearm a replacement. This scheduling state grants no eligibility, receipt or durable authority.

## Reading and notification

There is no public delivery ACK command, setting, timestamp or manual ACK wait. Complete original acceptance settles delivery and any frozen automatic-read fact in one application commit. Explicit reads remain independent and preserve their first times. Only Message Dispatches participate in Message reads; Inbox also contains typed Notices. Viewing a Web card is UI-local and does not mark it read.

Notification policy may create one typed Notice for eligible delivery timeout. Audit Event insertion or replay alone never creates native work. No delivery, read, reply or reservation proves that an agent accepted or completed a task.

## Native Message text

A Message uses one opening tag, one JSON metadata line, one `--` separator, the complete raw body and the closing tag:

```text
[AgentChat:Message]
<metadata JSON, including body_utf8_bytes>
--
<complete original body>
[/AgentChat:Message]
```

The UTF-8 byte count determines the body boundary, so marker lookalikes, NUL, CRLF and trailing whitespace remain body content. Saved DispatchV4 inputs with the former outer separators remain readable without rewriting their bytes or receipt digests. New output always uses the compact layout. Earlier policy revisions remain unsupported; typed Notice framing is separate.

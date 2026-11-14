"""Inspect committed canonical receipt facts without inferring native arrival."""

from frame_audit import AuditError, identifier, require, sequence


def canonical_row(row):
    require(isinstance(row, dict) and set(row) == {"recipient", "delivery", "reads"},
            "Unexpected or duplicate receipt projection")
    recipient = row["recipient"]
    require(isinstance(recipient, dict) and set(recipient) == {"actor_id", "kind", "handle"},
            "Invalid receipt identity")
    identifier(recipient["actor_id"])
    require(recipient["kind"] in ("agent", "user"), "Unknown receipt actor kind")
    require(isinstance(recipient["handle"], str) and recipient["handle"], "Missing handle")
    reads = row["reads"]
    require(isinstance(reads, list) and len(reads) <= 2, "Unbounded or duplicate read kinds")
    kinds = []
    for read in reads:
        require(isinstance(read, dict) and set(read) == {"kind", "at"}, "Invalid read fact")
        require(read["kind"] in ("automatic", "explicit"), "Unknown read kind")
        require(isinstance(read["at"], str) and bool(read["at"]), "Missing first read time")
        kinds.append(read["kind"])
    require(kinds in ([], ["automatic"], ["explicit"], ["automatic", "explicit"]),
            "Read facts are duplicated or out of order")
    delivery = row["delivery"]
    if delivery is None:
        require("automatic" not in kinds, "No native delivery for an automatic read")
        return row
    require(recipient["kind"] == "agent", "Anonymous users cannot receive native delivery")
    require(isinstance(delivery, dict) and set(delivery) == {
        "delivery_id", "state", "accepted_at", "acknowledged_at", "confirmation",
    }, "Invalid delivery facts")
    identifier(delivery["delivery_id"])
    require(delivery["state"] in ("pending", "inflight", "acknowledged", "cancelled"),
            "Unknown delivery state")
    for key in ("accepted_at", "acknowledged_at"):
        require(delivery[key] is None or isinstance(delivery[key], str) and bool(delivery[key]),
                "Invalid committed timestamp")
    require("automatic" not in kinds or delivery["accepted_at"] is not None,
            "Automatic read lacks durable native acceptance")
    confirmation = delivery["confirmation"]
    if confirmation is not None:
        require(isinstance(confirmation, dict) and set(confirmation) == {"deadline", "state"},
                "Invalid confirmation projection")
        require(isinstance(confirmation["deadline"], str) and bool(confirmation["deadline"]),
                "Missing original confirmation deadline")
        require(confirmation["state"] in ("waiting", "confirmed", "timed_out", "cancelled"),
                "Unknown confirmation state")
    # Deliberately do not infer accepted_at from ACK or force timed_out back to
    # confirmed when late native acceptance becomes known.
    return row


def detail_page(detail, message_id):
    require(isinstance(detail, dict) and set(detail) == {"message", "receipts"},
            "Old or duplicate public receipt API")
    message = detail["message"]
    require(isinstance(message, dict) and message.get("id") == message_id,
            "Receipt page belongs to another original message")
    require(not set(message).intersection({"readers", "read_at", "first_read_at", "unread",
                                           "read_confirmations"}), "Old message receipt alias")
    page = detail["receipts"]
    require(isinstance(page, dict) and set(page) == {"items", "next_cursor", "scan_cursor", "head"},
            "Invalid finite receipt page")
    require(isinstance(page["items"], list) and len(page["items"]) <= 200, "Unbounded receipt page")
    for key in ("scan_cursor", "head"):
        sequence(page[key])
    require(int(page["scan_cursor"]) <= int(page["head"]), "Scan cursor exceeds snapshot")
    if page["next_cursor"] is not None:
        sequence(page["next_cursor"])
        require(int(page["next_cursor"]) <= int(page["head"]), "Continuation exceeds snapshot")
    actors = set()
    for row in page["items"]:
        canonical_row(row)
        actor = row["recipient"]["actor_id"]
        require(actor not in actors, "An actor appears twice on one receipt page")
        actors.add(actor)
    return page


def require_facts(row, *, accepted, acknowledged, kinds, confirmation=None):
    canonical_row(row)
    delivery = row["delivery"]
    require(delivery is not None, "Expected an actual original delivery")
    require((delivery["accepted_at"] is not None) == accepted, "Unexpected acceptance fact")
    require((delivery["acknowledged_at"] is not None) == acknowledged, "Unexpected ACK fact")
    require([read["kind"] for read in row["reads"]] == kinds, "Unexpected per-kind read facts")
    if confirmation is not None:
        require(delivery["confirmation"] is not None
                and delivery["confirmation"]["state"] == confirmation,
                "Unexpected terminal confirmation history")


def no_positive_receipt_events(events, *, after):
    sequence(after)
    for event in events:
        sequence(event["seq"])
        if int(event["seq"]) > int(after):
            require(event["kind"] not in ("message.read", "message.ack", "message.acknowledged"),
                    "New positive receipt notification")

"""Validate one retained current native frame without inferring acceptance.

The campaign must separately bind the original owner, root, generation, delivery
and native result. A rendered frame or this decoder is never a native receipt.
"""

from __future__ import annotations

from dataclasses import dataclass
import hashlib
import json
import uuid


class AuditError(ValueError):
    """An observed fixture frame fails the closed current contract."""


def require(condition, message):
    if not condition:
        raise AuditError(message)


def unique_pairs(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "Duplicate native metadata field")
        result[key] = value
    return result


def decode_json(text):
    def invalid_constant(_value):
        raise AuditError("Non-finite JSON is not native metadata")
    try:
        return json.loads(text, object_pairs_hook=unique_pairs, parse_constant=invalid_constant)
    except (ValueError, TypeError) as error:
        raise AuditError("Invalid native metadata JSON") from error


def identifier(value):
    try:
        parsed = uuid.UUID(value)
    except (ValueError, TypeError, AttributeError) as error:
        raise AuditError("Invalid native identity") from error
    require(parsed.int != 0 and str(parsed) == value, "Noncanonical native identity")


def sequence(value):
    require(isinstance(value, str) and value.isascii() and value.isdecimal(), "Invalid sequence")
    require(str(int(value)) == value and 0 <= int(value) <= 2**63 - 1, "Noncanonical sequence")


@dataclass(frozen=True)
class Frame:
    """Exact observed frame; its digest still needs an independent native result."""

    kind: str
    metadata: dict
    body: str | None
    sha256: str

    def require_policy(self, auto_ack, auto_read):
        require(self.metadata["auto_ack"] is auto_ack, "Wrong frozen ACK policy")
        require(self.metadata["auto_read"] is auto_read, "Wrong frozen read policy")


def decode_frame(text):
    require(isinstance(text, str), "Native frame must be text")
    prefix = "[AgentChat:Message]\n--\n"
    suffix = "\n--\n[/AgentChat:Message]"
    body = None
    if text.startswith(prefix):
        kind = "message"
        header, separator, remainder = text[len(prefix):].partition("\n--\n")
        require(bool(separator), "Missing message metadata separator")
        metadata = decode_json(header)
        require(isinstance(metadata, dict), "Message metadata must be an object")
        require(set(metadata) == {
            "project_id", "generation", "delivery_id", "auto_ack", "auto_read",
            "event_id", "seq", "message_id", "thread_id", "author", "channel_id",
            "channel", "addressed", "body_utf8_bytes",
        }, "Unexpected or missing message metadata")
        count = metadata["body_utf8_bytes"]
        require(type(count) is int and 1 <= count <= 65_536, "Invalid original body length")
        raw = remainder.encode("utf-8")
        require(raw[count:] == suffix.encode(), "Truncated or noncanonical message body")
        try:
            body = raw[:count].decode("utf-8")
        except UnicodeDecodeError as error:
            raise AuditError("Body length cuts a UTF-8 code point") from error
        require(json.dumps(metadata, ensure_ascii=False, separators=(",", ":"), sort_keys=True)
                == header, "Noncanonical message metadata")
        author = metadata["author"]
        require(isinstance(author, dict) and set(author) == {"actor_id", "kind", "handle"},
                "Invalid typed author")
        identifier(author["actor_id"])
        require(author["kind"] in ("agent", "user"), "Unknown actor kind")
        require(isinstance(author["handle"], str) and bool(author["handle"]), "Missing author")
        for key in ("event_id", "message_id", "thread_id", "channel_id"):
            identifier(metadata[key])
        sequence(metadata["seq"])
    else:
        kind = "event"
        prefix, suffix = "[AgentChat:Event]\n--\n", "\n--\n[/AgentChat:Event]"
        require(text.startswith(prefix) and text.endswith(suffix), "Unknown native frame")
        require(len(text.encode()) <= 8_192, "Oversized current event frame")
        header = text[len(prefix):-len(suffix)]
        metadata = decode_json(header)
        require(isinstance(metadata, dict) and set(metadata) == {
            "project_id", "generation", "delivery_id", "auto_ack", "auto_read",
            "event", "addressed", "data_omitted",
        }, "Unexpected or missing event metadata")
        require(json.dumps(metadata, ensure_ascii=False, separators=(",", ":")) == header,
                "Noncanonical event metadata")
        event = metadata["event"]
        require(isinstance(event, dict) and set(event) == {
            "version", "event_id", "seq", "kind", "occurred_at", "actor_id", "channel_id",
            "message_id", "data",
        }, "Unexpected or missing event envelope")
        require(type(event.get("version")) is int
                and event["version"] == 1, "Invalid event version")
        identifier(event.get("event_id"))
        sequence(event.get("seq"))
        require(isinstance(event.get("kind"), str) and 1 <= len(event["kind"].encode()) <= 256
                and not any(ord(char) < 32 or 127 <= ord(char) <= 159 for char in event["kind"])
                and event["kind"] != "message.created",
                "Event cannot impersonate a full message")
        for key in ("actor_id", "channel_id", "message_id"):
            if event[key] is not None:
                identifier(event[key])
        require(type(metadata["data_omitted"]) is bool, "Invalid omission flag")
        require(not metadata["data_omitted"] or event.get("data") is None, "False omission")
    for key in ("project_id", "generation", "delivery_id"):
        identifier(metadata[key])
    for key in ("auto_ack", "auto_read", "addressed"):
        require(type(metadata[key]) is bool, "Missing or non-boolean frozen metadata")
    return Frame(kind, metadata, body, hashlib.sha256(text.encode()).hexdigest())

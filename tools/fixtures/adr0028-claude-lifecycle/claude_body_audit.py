#!/usr/bin/env python3
"""Audit exact Claude native input/receipt correlation for one body campaign."""

import hashlib
import json
from pathlib import Path
import re
import sys
import uuid


MESSAGE = re.compile(
    r"\[AgentChat:Message\]\n--\n(\{.*?\})\n--\n(.*?)\n--\n"
    r"\[/AgentChat:Message\]",
    re.DOTALL,
)


def load(path: Path):
    return json.loads(path.read_text())


def cli_stdout(path: Path):
    artifact = load(path)
    if artifact["exit_code"] != 0:
        raise AssertionError(f"CLI artifact failed: {path.name}")
    return json.loads(artifact["stdout"])


def expected_bodies(platform: str, result: dict):
    passive_prefix = f"P2-{platform}-PASSIVE-MAX-65536. FILLER="
    addressed_prefix = (
        f"@{result['participants']['receiver']['handle']} P2-{platform}-MAX-65536. "
        "Accept this complete native "
        "body, but do not call any AgentChat tool and do not mark it read. FILLER="
    )
    return [
        passive_prefix + "b" * (65536 - len(passive_prefix.encode())),
        addressed_prefix + "m" * (65536 - len(addressed_prefix.encode())),
        f"P2-{platform}-PASSIVE-AFTER-" + "a" * 32,
        result["return"]["body"],
    ]


def final_snapshot(case: Path):
    candidates = sorted((case / "results").glob("061-sender-ack-*.json"))
    if not candidates:
        raise AssertionError("final sender acceptance snapshot is absent")
    return candidates[-1], load(candidates[-1])


def frames(value: dict):
    parsed = []
    for metadata, body in MESSAGE.findall(value["text"]):
        parsed.append((json.loads(metadata), body))
    return parsed


def uuid_string(value):
    uuid.UUID(value)
    return value


def current_binding(snapshot: dict, actor: str):
    """Compare receipt identity with independent native and application observations."""
    observed = snapshot["actors"][actor]
    native = observed["native"]["registration"]
    registration = native["lifecycle"]["registration"]
    context = registration["context"]
    whoami = observed["whoami"]["result"]
    agent = whoami["agent"]
    if (
        native["lifecycle"]["stage"] != "project"
        or native["reservation"]["root"] != registration["root"]
        or native["reservation"]["generation"] != registration["generation"]
        or context["host_session_id"] != registration["root"]
        or whoami["participation"] != "joined"
        or agent["joined"] is not True
        or agent["host_session_id"] != registration["root"]
        or whoami["context_id"] != context["context_id"]
        or agent["incarnation"] != context["incarnation"]
    ):
        raise AssertionError(f"{actor}: current native/application bindings disagree")
    return {
        "handle": agent["handle"],
        "agent_id": agent["id"],
        "root": registration["root"],
        "context_id": context["context_id"],
        "incarnation": context["incarnation"],
        "native_generation": registration["generation"],
    }


def audit_actor(
    actor: str,
    acceptance: dict,
    bodies: list[str],
    ids: list[str],
    addressed: list[bool],
    binding: dict,
):
    if not (len(bodies) == len(ids) == len(addressed)):
        raise AssertionError(f"{actor}: expected audit vectors differ in length")
    inputs = acceptance["inputs"]
    receipts = acceptance["receipts"]
    order = [item["token"] for item in acceptance["order"]]
    if len(order) != len(set(order)):
        raise AssertionError(f"{actor}: duplicate receipt order tokens")
    audited = []
    for body, message_id, is_addressed in zip(bodies, ids, addressed):
        matches = []
        for token, input_value in inputs.items():
            for offset, (metadata, frame_body) in enumerate(frames(input_value)):
                if frame_body == body and metadata.get("message_id") == message_id:
                    matches.append((token, input_value, metadata, offset))
        if len(matches) != 1:
            raise AssertionError(
                f"{actor}: expected one exact frame for {message_id}, got {len(matches)}"
            )
        token, input_value, metadata, frame_offset = matches[0]
        receipt = receipts.get(token)
        if receipt is None or receipt.get("accepted") is not True:
            raise AssertionError(f"{actor}: exact receipt is not accepted for {token}")
        shared = ("token", "context_id", "incarnation", "native_generation", "root")
        if any(input_value.get(key) != receipt.get(key) for key in shared):
            raise AssertionError(f"{actor}: input/receipt identity mismatch for {token}")
        event = ("delivery_id", "generation", "seq")
        if any(metadata.get(key) != receipt.get(key) for key in event):
            raise AssertionError(f"{actor}: frame/receipt event mismatch for {token}")
        digest = hashlib.sha256(input_value["text"].encode()).digest()
        if list(digest) != receipt.get("digest"):
            raise AssertionError(f"{actor}: exact frame digest mismatch for {token}")
        if metadata.get("body_utf8_bytes") != len(body.encode()):
            raise AssertionError(f"{actor}: body length mismatch for {message_id}")
        if metadata.get("addressed") is not is_addressed:
            raise AssertionError(f"{actor}: addressed flag mismatch for {message_id}")
        native = {
            "root": receipt["root"],
            "context_id": receipt["context_id"],
            "incarnation": receipt["incarnation"],
            "native_generation": receipt["native_generation"],
        }
        if native != {
            key: binding[key]
            for key in ("root", "context_id", "incarnation", "native_generation")
        }:
            raise AssertionError(f"{actor}: receipt does not match current registration")
        native_id = uuid_string(receipt["native_id"])
        uuid_string(metadata["delivery_id"])
        uuid_string(metadata["generation"])
        uuid_string(input_value["context_id"])
        uuid_string(input_value["incarnation"])
        uuid_string(input_value["root"])
        audited.append(
            {
                "message_id": message_id,
                "body_utf8_bytes": len(body.encode()),
                "body_sha256": hashlib.sha256(body.encode()).hexdigest(),
                "input_sha256": hashlib.sha256(
                    input_value["text"].encode()
                ).hexdigest(),
                "token": token,
                "receipt_index": order.index(token),
                "frame_offset": frame_offset,
                "native_id": native_id,
                "delivery_id": receipt["delivery_id"],
                "generation": receipt["generation"],
                "seq": receipt["seq"],
                "root": receipt["root"],
                "context_id": receipt["context_id"],
                "incarnation": receipt["incarnation"],
                "native_generation": receipt["native_generation"],
                "accepted": True,
            }
        )
    observed_order = [
        (item["receipt_index"], item["frame_offset"]) for item in audited
    ]
    if observed_order != sorted(observed_order):
        raise AssertionError(f"{actor}: audited native FIFO order changed")
    if len({item["native_id"] for item in audited}) != len(audited):
        raise AssertionError(f"{actor}: native receipt IDs are not unique")
    return audited


def main():
    case = Path(sys.argv[1]).resolve()
    result = load(case / "body-result.json")
    snapshot_path, snapshot = final_snapshot(case)
    terminal = load(case / "terminal.json")
    if (
        result["result"] != "pass"
        or result["profile_restoration_pending"] is not False
        or terminal["failure"] is not None
        or terminal["profile_restored"] is not True
        or terminal["real_config_unchanged"] is not True
        or terminal["live_native_owners"]
    ):
        raise AssertionError("campaign or owned cleanup did not pass")
    for actor, participant in (("update-receiver", "receiver"), ("update-sender", "sender")):
        if current_binding(snapshot, actor) != result["participants"][participant]:
            raise AssertionError(f"{actor}: recorded binding differs from final native registration")
    bodies = expected_bodies(result["platform"], result)
    receiver_baseline = cli_stdout(
        case / "artifacts" / "body-baseline-receiver-inbox.json"
    )
    sender_baseline = cli_stdout(
        case / "artifacts" / "body-return-sender-baseline-inbox.json"
    )
    if receiver_baseline["result"]["items"]:
        raise AssertionError("receiver baseline inbox was not empty")
    if sender_baseline["result"]["items"]:
        raise AssertionError("return sender baseline inbox was not empty")
    if result["roots"]["delivery"] != result["participants"]["receiver"]["root"]:
        raise AssertionError("delivery result root differs from receiver registration")
    mention = result["addressed_mention"]
    if (
        len(mention) != 1
        or mention[0]["handle"] != result["participants"]["receiver"]["handle"]
        or mention[0]["recipient_id"]
        != result["participants"]["receiver"]["agent_id"]
    ):
        raise AssertionError("addressed result differs from receiver registration")
    if mention[0]["state"] not in ("queued", "delivered"):
        raise AssertionError("addressed mention was not queued or delivered")
    if [len(body.encode()) for body in bodies[:3]] != result["body_bytes"]:
        raise AssertionError("recorded body lengths differ from deterministic inputs")
    if [hashlib.sha256(body.encode()).hexdigest() for body in bodies[:3]] != result[
        "body_sha256"
    ]:
        raise AssertionError("recorded body digests differ from deterministic inputs")
    receiver = audit_actor(
        "update-receiver",
        snapshot["actors"]["update-receiver"]["acceptances"],
        bodies[:3],
        result["message_ids"],
        [False, True, False],
        result["participants"]["receiver"],
    )
    sender = audit_actor(
        "update-sender",
        snapshot["actors"]["update-sender"]["acceptances"],
        bodies[3:],
        [result["return"]["message_id"]],
        [True],
        result["participants"]["sender"],
    )
    if [str(item["seq"]) for item in receiver] != [
        str(value) for value in result["message_sequences"]
    ]:
        raise AssertionError("receiver event sequences differ from post results")
    for actor, accepted in (("update-receiver", receiver), ("update-sender", sender)):
        delivery = snapshot["actors"][actor]["whoami"]["result"]["native_delivery"]
        if int(delivery["accepted_seq"]) < max(int(item["seq"]) for item in accepted):
            raise AssertionError(f"{actor}: exact native receipts did not advance application cursor")
    if snapshot["status"]["result"]["statistics"] != result["statistics"]["final"]:
        raise AssertionError("final statistics differ from the captured application snapshot")
    output = {
        "result": "pass",
        "scope": "four_body_acceptance_ack_read_and_return",
        "case": str(case),
        "source_snapshot": str(snapshot_path.relative_to(case)),
        "platform": result["platform"],
        "host_version": result["host_version"],
        "baseline_inbox_items": {"receiver": 0, "sender": 0},
        "receiver": receiver,
        "sender": sender,
        "remaining_native_delivery": {
            actor: snapshot["actors"][actor]["whoami"]["result"]["native_delivery"]
            for actor in ("update-receiver", "update-sender")
        },
        "cleanup": terminal,
    }
    destination = case / "body-audit.json"
    destination.write_text(json.dumps(output, indent=2) + "\n")
    print(json.dumps({"result": "pass", "artifact": str(destination)}))


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Drive one disposable current-Claude native full-body acceptance campaign."""

import hashlib
import json
from pathlib import Path
import subprocess
import sys
import time
import traceback

from claude_body_support import CampaignContext

driver = CampaignContext(Path(sys.argv[1]).resolve(), Path(sys.argv[2]).resolve())
case = driver.case
process = driver.process
wait_path = driver.wait_path
issue = driver.issue
cli = driver.cli
wait_idle = driver.wait_idle
require_participation = driver.require_participation
wait_accepted = driver.wait_accepted
wait_acknowledged = driver.wait_acknowledged
wait_delivery_ready = driver.wait_delivery_ready
result_items = driver.result_items


failure = None
try:
    ready = json.loads(wait_path(case / "ready.json").read_text())
    platform = ready["actors"]["update-receiver"]["context"]["os_profile"]
    for index, actor in enumerate(("update-sender", "update-receiver"), start=10):
        issue(
            f"{index:03d}-materialize-{actor}",
            {
                "kind": "prompt",
                "actor": actor,
                "text": "Call only the AgentChat MCP agentchat_whoami tool once. Report participation and exact binding. Do not call Join, post, read, or use shell/CLI.",
            },
        )
        require_participation(
            wait_idle(actor, f"{index + 1:03d}-{actor}"), actor, "joined"
        )
    issue("014-release-setup-observer", {"kind": "release-setup-observer"})

    receiver = "update-receiver"
    sender = "update-sender"
    sender_original_root = ready["actors"][sender]["root"]
    original_root = ready["actors"][receiver]["root"]
    issue("020-new-receiver-root", {"kind": "prompt", "actor": receiver, "text": "/new"})
    changed = issue("021-refresh-receiver-root", {"kind": "refresh-root", "actor": receiver})
    if changed["old_root"] != original_root or changed["new_root"] == original_root:
        raise AssertionError(changed)
    issue(
        "022-activate-receiver-root",
        {
            "kind": "prompt",
            "actor": receiver,
            "text": "This is the first normal input on this distinct root. Call only AgentChat agentchat_whoami once and report the exact joined binding. Do not call Join, post, read, or use shell/CLI.",
        },
    )
    require_participation(
        wait_idle(receiver, "023-receiver-root"), receiver, "joined"
    )
    baseline = wait_delivery_ready([receiver], "030-clean-receiver-boundary")
    sender_context = baseline["actors"]["update-sender"]["native"]["registration"][
        "lifecycle"
    ]["registration"]["context"]["context_id"]
    receiver_context = baseline["actors"][receiver]["native"]["registration"][
        "lifecycle"
    ]["registration"]["context"]["context_id"]
    receiver_agent = baseline["actors"][receiver]["whoami"]["result"]["agent"]
    receiver_handle = receiver_agent["handle"]
    baseline_stats = baseline["status"]["result"]["statistics"]
    receiver_baseline_inbox = cli(
        "baseline-receiver-inbox", ["inbox"], context=receiver_context
    )
    if result_items(receiver_baseline_inbox):
        raise AssertionError("receiver baseline retained a pending native candidate")

    passive_prefix = f"P2-{platform}-PASSIVE-MAX-65536. FILLER="
    passive_before = passive_prefix + "b" * (
        65536 - len(passive_prefix.encode())
    )
    instruction = (
        f"@{receiver_handle} P2-{platform}-MAX-65536. Accept this complete native body, "
        "but do not call any AgentChat tool and do not mark it read. FILLER="
    )
    maximum = instruction + "m" * (65536 - len(instruction.encode()))
    passive_after = f"P2-{platform}-PASSIVE-AFTER-" + "a" * 32
    if len(passive_before.encode()) != 65536 or len(maximum.encode()) != 65536:
        raise AssertionError("maximum bodies are not exactly 65,536 UTF-8 bytes")
    bodies = [passive_before, maximum, passive_after]
    posted = []
    for index, body in enumerate(bodies, start=1):
        value = cli(
            f"post-{index}",
            ["post", "--channel", "dev", "--stdin"],
            context=sender_context,
            body=body,
        )
        posted.append(value["result"])
    posted_ids = [value["message_id"] for value in posted]
    mention = posted[1]["mention_results"]
    if (
        len(mention) != 1
        or mention[0]["handle"] != receiver_handle
        or mention[0]["recipient_id"] != receiver_agent["id"]
        or mention[0]["state"] not in ("queued", "delivered")
        or posted[1]["warnings"]
    ):
        raise AssertionError("maximum addressed body did not target the current root")

    midpoint, receiver_order = wait_accepted(receiver, bodies, "040-receiver")
    midpoint = wait_acknowledged(
        baseline_stats["acknowledged_deliveries"] + 3, "041-receiver"
    )
    midpoint_stats = midpoint["status"]["result"]["statistics"]
    if midpoint_stats["acknowledged_deliveries"] < baseline_stats["acknowledged_deliveries"] + 3:
        raise AssertionError("native acceptance did not acknowledge every posted body")
    if midpoint_stats["read_receipts"] != baseline_stats["read_receipts"]:
        raise AssertionError("native ACK created an explicit read")
    for index, message_id in enumerate(posted_ids, start=1):
        detail = cli(f"mid-message-{index}", ["message", message_id])
        if detail["result"]["readers"]["items"]:
            raise AssertionError("native acceptance created a reader")
    inbox = cli("mid-receiver-inbox", ["inbox"], context=receiver_context)
    pending_message_ids = {
        item["event"].get("message_id") for item in result_items(inbox)
    }
    if pending_message_ids.intersection(posted_ids):
        raise AssertionError("accepted native body remained in the inbox")

    root = posted_ids[1]
    issue(
        "044-explicit-read",
        {
            "kind": "prompt",
            "actor": receiver,
            "text": (
                "Call only AgentChat agentchat_read once. Set message_ids to exactly "
                f"[{root}]. Do not call any other tool and do not use shell/CLI."
            ),
        },
    )
    wait_idle(receiver, "044-explicit-read")
    read_detail = cli("after-explicit-read", ["message", root])
    if [item["reader"]["handle"] for item in read_detail["result"]["readers"]["items"]] != [
        receiver_handle
    ]:
        raise AssertionError("the exact maximum message read was not committed")

    issue("045-new-sender-root", {"kind": "prompt", "actor": sender, "text": "/new"})
    sender_changed = issue(
        "046-refresh-sender-root", {"kind": "refresh-root", "actor": sender}
    )
    if (
        sender_changed["old_root"] != sender_original_root
        or sender_changed["new_root"] == sender_original_root
    ):
        raise AssertionError(sender_changed)
    issue(
        "047-activate-sender-root",
        {
            "kind": "prompt",
            "actor": sender,
            "text": "This is the first normal input on this distinct root. Call only AgentChat agentchat_whoami once and report the exact joined binding. Do not call Join, post, read, or use shell/CLI.",
        },
    )
    require_participation(
        wait_idle(sender, "048-sender-root"), sender, "joined"
    )
    return_boundary = wait_delivery_ready(
        ["update-sender"], "049-clean-sender-boundary"
    )
    sender_agent = return_boundary["actors"]["update-sender"]["whoami"]["result"][
        "agent"
    ]
    sender_handle = sender_agent["handle"]
    sender_context = return_boundary["actors"]["update-sender"]["native"][
        "registration"
    ]["lifecycle"]["registration"]["context"]["context_id"]
    sender_baseline_inbox = cli(
        "return-sender-baseline-inbox", ["inbox"], context=sender_context
    )
    if result_items(sender_baseline_inbox):
        raise AssertionError("return sender baseline retained a pending native candidate")
    return_body = f"@{sender_handle} P2-{platform}-CLAUDE-RETURN-OK"
    issue(
        "050-return-after-explicit-read",
        {
            "kind": "prompt",
            "actor": receiver,
            "text": (
                "Call only AgentChat agentchat_reply once. Reply to root_message_id "
                f"{root}; omit read_message_ids; set body exactly {return_body!r}. "
                "Do not call any other tool and do not use shell/CLI."
            ),
        },
    )
    wait_idle(receiver, "051-return")
    deadline = time.monotonic() + 240
    reply = None
    while time.monotonic() < deadline:
        history = cli("history-" + str(time.monotonic_ns()), ["history", "--channel", "dev"])
        matches = [item for item in result_items(history) if item["body"] == return_body]
        if len(matches) == 1:
            reply = matches[0]
            break
        time.sleep(1)
    if reply is None or reply["root_message_id"] != root:
        raise AssertionError("exact Claude return reply was not committed")

    final, sender_order = wait_accepted("update-sender", [return_body], "060-sender")
    final = wait_acknowledged(
        midpoint_stats["acknowledged_deliveries"] + 1, "061-sender"
    )
    final_stats = final["status"]["result"]["statistics"]
    detail = cli("final-max-message", ["message", root])
    readers = detail["result"]["readers"]["items"]
    if [item["reader"]["handle"] for item in readers] != [receiver_handle]:
        raise AssertionError("the exact maximum message read was not retained")
    history = cli("final-history", ["history", "--channel", "dev"])
    items = result_items(history)
    expected = posted_ids + [reply["id"]]
    actual = [item["id"] for item in items if item["id"] in expected]
    if actual != expected:
        raise AssertionError("message/reply creation order changed")
    summary = {
        "result": "pass",
        "case": str(case),
        "platform": platform,
        "host_version": "2.1.310",
        "roots": {
            "receiver_original": original_root,
            "delivery": changed["new_root"],
            "sender_original": sender_original_root,
            "return": sender_changed["new_root"],
        },
        "participants": {
            "receiver": {
                "handle": receiver_handle,
                "agent_id": receiver_agent["id"],
                "root": receiver_agent["host_session_id"],
                "context_id": receiver_context,
                "incarnation": receiver_agent["incarnation"],
                "native_generation": baseline["actors"][receiver]["native"][
                    "registration"
                ]["lifecycle"]["registration"]["generation"],
            },
            "sender": {
                "handle": sender_handle,
                "agent_id": sender_agent["id"],
                "root": sender_agent["host_session_id"],
                "context_id": sender_context,
                "incarnation": sender_agent["incarnation"],
                "native_generation": return_boundary["actors"]["update-sender"][
                    "native"
                ]["registration"]["lifecycle"]["registration"]["generation"],
            },
        },
        "addressed_mention": mention,
        "message_ids": posted_ids,
        "message_sequences": [value["seq"] for value in posted],
        "body_bytes": [len(body.encode()) for body in bodies],
        "body_sha256": [hashlib.sha256(body.encode()).hexdigest() for body in bodies],
        "receiver_native_order": [
            {"receipt_index": index, "frame_offset": offset, "token": token}
            for index, offset, token in receiver_order
        ],
        "return": {
            "message_id": reply["id"],
            "root_message_id": reply["root_message_id"],
            "body": return_body,
            "sender_native_order": sender_order,
        },
        "statistics": {
            "baseline": baseline_stats,
            "after_native_acceptance_before_read": midpoint_stats,
            "final": final_stats,
        },
        "readers_after_acceptance": [],
        "readers_after_explicit_reply": [receiver_handle],
        "profile_restoration_pending": True,
    }
    (case / "body-result.json").write_text(json.dumps(summary, indent=2) + "\n")
except BaseException as error:
    failure = f"{type(error).__name__}: {error}"
    case.with_suffix(".body-driver-failure.txt").write_text(traceback.format_exc())
finally:
    try:
        if process.poll() is None and (case / "commands").exists():
            issue("999-stop", {"kind": "stop"}, timeout=30)
    except BaseException as error:
        failure = (failure + "; " if failure else "") + f"stop: {error!r}"
    try:
        code = process.wait(timeout=120)
    except subprocess.TimeoutExpired:
        process.terminate()
        code = process.wait(timeout=30)
        failure = (failure + "; " if failure else "") + "rig required termination"
    terminal = case / "terminal.json"
    cleanup = json.loads(terminal.read_text()) if terminal.exists() else None
    if not cleanup or cleanup.get("failure") or not cleanup.get("profile_restored"):
        failure = (failure + "; " if failure else "") + "fixture cleanup did not pass"
    result_path = case / "body-result.json"
    if result_path.exists():
        value = json.loads(result_path.read_text())
        value["profile_restoration_pending"] = False
        value["cleanup"] = cleanup
        result_path.write_text(json.dumps(value, indent=2) + "\n")
    summary = {"case": str(case), "rig_exit": code, "failure": failure}
    case.with_suffix(".body-driver-result.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary), flush=True)
    raise SystemExit(1 if failure or code else 0)

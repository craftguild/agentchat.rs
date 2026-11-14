#!/usr/bin/env python3
"""Verify one warm Claude stream through a peer departure and its successor."""

import hashlib
import json
from pathlib import Path
import sys
import time
import traceback

from claude_body_audit import current_binding
from claude_body_support import CampaignContext


def accepted(snapshot, actor, context, predicate):
    """Correlate the exact native result with its complete original frame."""
    values = snapshot["actors"][actor]["acceptances"]
    found = []
    for token, value in values["inputs"].items():
        if value["context_id"] != context:
            continue
        envelope = json.loads(value["text"].split("\n")[2])
        if not predicate(envelope):
            continue
        receipt = values["receipts"].get(token)
        if receipt is None:
            continue
        event = envelope.get("event", envelope)
        assert receipt["accepted"] is True
        assert receipt["native_id"]
        assert list(hashlib.sha256(value["text"].encode()).digest()) == receipt["digest"]
        for key in ("root", "context_id", "incarnation", "native_generation", "token"):
            assert value[key] == receipt[key]
        for key in ("delivery_id", "generation"):
            assert envelope[key] == receipt[key]
        assert event["seq"] == receipt["seq"]
        found.append({"input": value, "receipt": receipt})
    assert len(found) <= 1, "one exact delivery must have one original native result"
    return found[0] if found else None


def main():
    driver = CampaignContext(Path(sys.argv[1]).resolve(), Path(sys.argv[2]).resolve())
    case = driver.case
    receiver, sender = "update-receiver", "update-sender"
    result, failure = None, None
    try:
        ready = json.loads(driver.wait_path(case / "ready.json").read_text())
        for index, actor in enumerate((sender, receiver), 10):
            driver.issue(f"{index:03d}-materialize-{actor}", {
                "kind": "prompt", "actor": actor,
                "text": "Call only AgentChat agentchat_whoami once. Report the current joined binding. Do not call Join, post, read, or shell/CLI.",
            })
            driver.require_participation(driver.wait_idle(actor, f"{index:03d}-materialized"), actor, "joined")
        driver.issue("014-release-observer", {"kind": "release-setup-observer"})
        driver.issue("020-new-receiver", {"kind": "prompt", "actor": receiver, "text": "/new"})
        transition = driver.issue("021-refresh-receiver", {"kind": "refresh-root", "actor": receiver})
        assert transition["old_root"] == ready["actors"][receiver]["root"]
        assert transition["new_root"] != transition["old_root"]
        driver.issue("022-activate-receiver", {
            "kind": "prompt", "actor": receiver,
            "text": "Call only AgentChat agentchat_whoami once. Report the joined binding. Do not call Join, post, read, or shell/CLI.",
        })
        driver.wait_idle(receiver, "023-receiver")
        baseline = driver.wait_delivery_ready([receiver], "024-baseline")
        binding = current_binding(baseline, receiver)
        context = binding["context_id"]
        sender_context = current_binding(baseline, sender)["context_id"]
        assert not driver.cli("baseline-inbox", ["inbox"], context=context)["result"]["items"]
        warm = f"@{binding['handle']} AM6-WARM-ONE. Reply exactly AM6-WARM-OK; do not call tools."
        warm_post = driver.cli("warm-post", ["post", "--channel", "dev", "--stdin"], context=sender_context, body=warm)["result"]
        driver.wait_accepted(receiver, [warm], "030-warm")
        warm_snapshot = driver.wait_acknowledged(1, "031-warm")
        assert current_binding(warm_snapshot, receiver) == binding
        driver.issue("040-new-sender", {"kind": "prompt", "actor": sender, "text": "/new"})
        peer = driver.issue("041-refresh-sender", {"kind": "refresh-root", "actor": sender})
        assert peer["old_root"] == ready["actors"][sender]["root"]
        assert peer["new_root"] != peer["old_root"]
        driver.issue("042-activate-sender", {
            "kind": "prompt", "actor": sender,
            "text": "Call only AgentChat agentchat_whoami once. Report the joined binding. Do not call Join, post, read, or shell/CLI.",
        })
        before_successor = driver.wait_idle(sender, "043-sender")
        assert current_binding(before_successor, receiver) == binding
        sender_context = current_binding(before_successor, sender)["context_id"]
        body = f"@{binding['handle']} AM6-SUCCESSOR-ONE. Reply exactly AM6-SUCCESSOR-OK; do not call tools."
        posted = driver.cli("successor-post", ["post", "--channel", "dev", "--stdin"], context=sender_context, body=body)["result"]
        assert posted["mention_results"][0]["recipient_id"] == binding["agent_id"]
        assert posted["mention_results"][0]["state"] in ("queued", "delivered")
        assert not posted["warnings"]
        final, _ = driver.wait_accepted(receiver, [body], "050-successor")
        deadline = time.monotonic() + 30
        while final["actors"][receiver]["whoami"]["result"]["native_delivery"]["head"] is not None:
            assert time.monotonic() < deadline, "accepted successor did not settle its exact head"
            driver.counter += 1
            final = driver.issue(f"060-settled-{driver.counter:03d}", {"kind": "snapshot"})
            time.sleep(0.2)
        assert current_binding(final, receiver) == binding
        selectors = (
            lambda e: e.get("message_id") == warm_post["message_id"],
            lambda e: e.get("event", {}).get("kind") == "agent.launch_retired",
            lambda e: e.get("message_id") == posted["message_id"],
        )
        frames = [accepted(final, receiver, context, selector) for selector in selectors]
        assert all(frames), "warm, departure and successor must each have an exact receipt"
        for frame in frames:
            for key in ("root", "context_id", "incarnation", "native_generation"):
                assert frame["receipt"][key] == binding[key]
        sequences = [int(frame["receipt"]["seq"]) for frame in frames]
        assert sequences == sorted(set(sequences)), "native FIFO order changed"
        for frame, expected in ((frames[0], warm), (frames[2], body)):
            assert frame["input"]["text"].split("\n--\n", 2)[2].rsplit("\n--\n", 1)[0] == expected
        stats = final["status"]["result"]["statistics"]
        assert stats["read_receipts"] == baseline["status"]["result"]["statistics"]["read_receipts"]
        delivery = final["actors"][receiver]["whoami"]["result"]["native_delivery"]
        assert int(delivery["accepted_seq"]) >= sequences[-1]
        result = {
            "result": "pass", "scope": "current_production_peer_notice_then_successor",
            "case": str(case), "binding": binding, "peer_transition": peer,
            "frames": frames, "sequences": sequences, "statistics": stats,
            "final_delivery": delivery, "manual_ack_or_receiver_rejoin": False,
        }
    except BaseException as error:
        failure = {"error": repr(error), "traceback": traceback.format_exc()}
    finally:
        if driver.process.poll() is None:
            driver.issue("999-stop", {"kind": "stop"}, timeout=30)
        code = driver.process.wait(timeout=120)
        terminal = json.loads((case / "terminal.json").read_text())
        if code or terminal["failure"] or not terminal["profile_restored"] or not terminal["real_config_unchanged"] or terminal["live_native_owners"]:
            failure = failure or {"error": "owned cleanup failed", "terminal": terminal, "rig_exit": code}
        output = result or {"result": "failed"}
        output.update(failure=failure, cleanup=terminal)
        destination = case / "notice-result.json"
        destination.write_text(json.dumps(output, indent=2) + "\n")
        print(json.dumps({"result": output["result"], "failure": failure, "artifact": str(destination)}), flush=True)
    return 1 if failure else 0


if __name__ == "__main__":
    raise SystemExit(main())

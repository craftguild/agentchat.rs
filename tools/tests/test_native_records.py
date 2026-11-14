"""Reject false native acceptance caused by inherited or incomplete results."""

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("native_records", Path(__file__).parents[1] / "native_records.py")
native = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(native)


def pair(identifier, operation, context, result, **arguments):
    inputs = {"context_id": context, **arguments} if context else arguments
    use = {"type": "tool_use", "id": identifier, "name": "mcp__agentchat__agentchat_" + operation,
           "input": inputs}
    envelope = {"version": 1, "project_id": "project", "request_id": identifier,
                "result": result}
    output = {"type": "tool_result", "tool_use_id": identifier,
              "content": json.dumps(envelope)}
    return [{"sessionId": "fork", "message": {"content": [use]}},
            {"sessionId": "fork", "message": {"content": [output]}}]


def original(context="old"):
    return (pair("body", "message", context, {"message": {"id": "message", "body": "complete"}},
                 message_id="message")
            + pair("ack", "delivery_ack", context, {"acknowledged": ["delivery"]},
                   delivery_ids=["delivery"])
            + pair("read", "read", context, {"changed": ["message"]}, message_ids=["message"]))


class NativeRecordsTests(unittest.TestCase):
    def decode(self, rows, **options):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "native.jsonl"
            path.write_text("".join(json.dumps(v) + "\n" for v in rows))
            return native.read(path, "fork", **options)

    def expected(self):
        return {"message_id": "message", "body": "complete", "delivery_ids": ["delivery"]}

    def test_inherited_fork_receipts_cannot_prove_current_participation(self):
        inherited = original()
        current = pair("current", "status", None,
                       {"binding": {"context_id": "new", "participation": "unjoined", "agent": None}})
        values = self.decode(inherited + current)
        self.assertEqual([v["tool_use_id"] for v in native.scoped(values, "new")], ["current"])
        with self.assertRaises(native.EvidenceError):
            native.receipt(values, "new", "project", self.expected())

    def test_earlier_body_is_explicit_but_ack_and_read_must_be_current(self):
        values = self.decode(original() + pair("new-ack", "delivery_ack", "new",
                             {"acknowledged": ["delivery"]}, delivery_ids=["delivery"])
                             + pair("new-read", "read", "new", {"changed": ["message"]},
                                    message_ids=["message"]))
        with self.assertRaises(native.EvidenceError):
            native.receipt(values, "new", "project", self.expected())
        result = native.receipt(values, "new", "project", self.expected(), ["old"])
        self.assertEqual(result["ack"][0]["tool_use_id"], "new-ack")
        with self.assertRaises(native.EvidenceError):
            native.receipt(self.decode(original()), "new", "project", self.expected(), ["old"])

    def test_complete_structured_result_survives_display_truncation(self):
        rows = original("new")
        envelope = json.loads(rows[1]["message"]["content"][0]["content"])
        rows[1]["message"]["content"][0]["content"] = "truncated display"
        rows[1]["toolUseResult"] = {"structuredContent": envelope}
        values = self.decode(rows + rows)
        self.assertEqual(len(values), 3)
        self.assertTrue(native.receipt(values, "new", "project", self.expected()))

    def test_disagreeing_complete_envelopes_are_not_accepted(self):
        rows = original()
        rows[1]["toolUseResult"] = {"structuredContent": {
            "version": 1, "project_id": "other", "result": {}}}
        with self.assertRaises(native.EvidenceError):
            self.decode(rows)

    def test_unknown_outcome_and_native_error_do_not_prove_read(self):
        for text, error in [
                ("[Tool call interrupted: the session ended before this call's result was recorded]", False),
                ("agentchat error summary: " + json.dumps({"request_id": "r", "code": "session_required"}), True)]:
            with self.subTest(text=text):
                rows = original("new")
                rows[-1]["message"]["content"][0].update(content=text, is_error=error)
                values = self.decode(rows)
                with self.assertRaises(native.EvidenceError):
                    native.receipt(values, "new", "project", self.expected())

    def test_body_from_another_message_cannot_satisfy_original(self):
        rows = original("new")
        envelope = json.loads(rows[1]["message"]["content"][0]["content"])
        envelope["result"]["message"]["id"] = "different-message"
        rows[1]["message"]["content"][0]["content"] = json.dumps(envelope)
        with self.assertRaises(native.EvidenceError):
            native.receipt(self.decode(rows), "new", "project", self.expected())

    def test_root_size_and_conflicting_call_inputs_are_rejected(self):
        rows = original()
        rows[0]["sessionId"] = "other-root"
        with self.assertRaises(native.EvidenceError):
            self.decode(rows)
        with self.assertRaises(native.EvidenceError):
            self.decode(original(), limit=10)
        changed = pair("body", "message", "new", {}, message_id="different")
        with self.assertRaises(native.EvidenceError):
            self.decode(original() + changed)

    def test_input_binding_context_disagreement_is_rejected(self):
        values = self.decode(pair("status", "status", "old",
                             {"binding": {"context_id": "new"}}))
        with self.assertRaises(native.EvidenceError):
            native.scoped(values, "new")


if __name__ == "__main__":
    unittest.main()

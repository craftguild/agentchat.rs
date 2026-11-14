"""Original full-input recovery cut on the explicitly owned AgentChat runtime.

Native owners and the shared daemon are never signalled. A stopped runtime is
killed only after the current helper's acceptance marker, actual complete native
frame, original process birth and all live native owners correlate. These audits
do not replace the product's private receipt/provenance validator.
"""

import copy
import json

from cli_adapter import CallError, save_record
from frame_audit import decode_frame, identifier, require
from owner_facts import facts, matches, stopped
from receipt_audit import canonical_row, require_facts


def correlate_hold(marker, frame_text, *, project_id, context, generation):
    frame = decode_frame(frame_text)
    require(marker.get("native_accepted") is True and marker.get("core_completion_held") is True,
            "No actual accepted-before-completion cut")
    require(marker.get("receipt_only") is False and marker.get("admitted") is False,
            "The cut must be the first actual native entry, not a receipt-only poll")
    identifier(marker["token"])
    require(type(marker["attempt"]) is int and marker["attempt"] == 1, "The cut is not the original attempt")
    require(frame.kind == "message" and marker["context"] == context,
            "Acceptance marker does not belong to the original selected descriptor")
    for key in ("project_id", "generation", "message_id", "delivery_id", "seq"):
        expected = project_id if key == "project_id" else generation if key == "generation" else marker[key]
        require(frame.metadata[key] == expected, "Original native frame and held dispatch differ")
    require(marker["generation"] == generation, "Held generation changed")
    return frame


def correlate_claude_original(file, receipt, marker, frame):
    require(type(file.get("version")) is int and file["version"] == 3,
            "Fresh campaign requires its current original input capsule")
    require(type(receipt.get("version")) is int and receipt["version"] in (1, 2)
            and receipt.get("accepted") is True, "No positive original native receipt")
    require(file["text"] is not None and decode_frame(file["text"]).sha256 == frame.sha256,
            "Original saved text digest differs from actual native body")
    require(file["token"] == marker["token"], "Original receipt belongs to another dispatch token")
    require(file["root"] == marker["context"]["host_session_id"]
            and file["context_id"] == marker["context"]["context_id"]
            and file["incarnation"] == marker["context"]["incarnation"], "Original provenance changed")
    for key in ("root", "native_generation", "context_id", "incarnation", "token"):
        require(receipt[key] == file[key], "Receipt does not match the whole original capsule")
    for key in ("generation", "delivery_id", "seq"):
        require(receipt[key] == frame.metadata[key], "Original receipt frame correlation failed")
    identifier(receipt["native_id"])
    digest = receipt["digest"]
    require(isinstance(digest, list) and len(digest) == 32
            and all(type(value) is int and 0 <= value <= 255 for value in digest)
            and bytes(digest).hex() == frame.sha256, "Original receipt lost its exact text checksum")


def recovered_facts(host, marker, frame, row, progress, *, original_native_count):
    canonical_row(row)
    require(original_native_count == 1, "Original input was absent or resubmitted")
    require(row["delivery"] is not None and row["delivery"]["delivery_id"] == marker["delivery_id"],
            "Recovered facts belong to a different original delivery")
    require(progress["generation"] == marker["generation"], "Recovery created a replacement generation")
    if host == "claude":
        ack, read = frame.metadata["auto_ack"], frame.metadata["auto_read"]
        require_facts(row, accepted=True, acknowledged=ack, kinds=["automatic"] if read else [])
        if ack:
            require(progress["state"] == "ready" and progress["head"] is None,
                    "Original positive receipt did not settle its auto-ACK head")
        else:
            require(progress["state"] == "awaiting_ack" and progress["head"]["delivery_id"] == marker["delivery_id"],
                    "Original manual-ACK head was not retained after receipt settlement")
    elif host == "codex":
        require(row["delivery"]["accepted_at"] is None and not row["reads"],
                "Codex cannot invent a durable receipt after the held native result is lost")
        head = progress["head"]
        require(progress["state"] == "acceptance_unknown" and head is not None,
                "Unknown original acceptance must remain suspended")
        require(head["delivery_id"] == marker["delivery_id"] and head["seq"] == marker["seq"]
                and head["attempts"] == marker["attempt"] and head["accepted_at"] is None,
                "Recovery replaced the original Unknown head")
    else:
        raise CallError("Unknown native recovery host")


class RecoveryController:
    def __init__(self, setup, phase, producers, observers):
        self.setup, self.phase = setup, phase
        self.producers, self.observers = producers, observers
        self.marker_path = setup.plan.state / "original-acceptance-held.json"
        self.before = None
        self.marker = self.frame = None
        self.native_text = None
        self.cut_record = None
        self.restart_label = None

    def arm(self):
        if self.before is not None or self.marker_path.exists():
            raise CallError("This original-receipt cut cannot be armed or replayed twice")
        before = self.phase.settled()
        for name, observer in self.observers.items():
            observation = observer.observe(self.phase.label("recovery-idle-" + name))
            require(observation["phase"] == "idle" and not observation["pending_requests"],
                    "Original native work/permission must finish before the recovery cut")
        if self.setup.viewer:
            self.setup.viewer.stop(interrupt=True)
        self.setup.runtime.stop()
        self.before = before
        save_record(self.setup.plan.artifacts / "recovery-before.json", before, exclusive=True)
        self.setup.start_runtime(hold_accepted=self.marker_path, label="runtime-recovery-armed")
        self.restart_label = "recovery-setup-viewer"

    def ready(self):
        if not self.setup.poll_runtime_ready():
            return False
        if self.restart_label:
            self.setup.start_setup_observer(label=self.restart_label)
            self.restart_label = None
        return True

    def cut(self, receiver, native_text, *, claude_original=None):
        if self.before is None or self.cut_record is not None:
            raise CallError("No armed original cut, or it was already consumed")
        producer = self.producers[receiver]
        require(producer.readiness(), "No current original-owner readiness at the cut")
        marker = json.loads(self.marker_path.read_text())
        generation = self.before[producer.name]["native_delivery"]["generation"]
        frame = correlate_hold(marker, native_text, project_id=self.setup.client.project_id,
                               context=producer.context, generation=generation)
        if producer.host == "claude":
            require(claude_original is not None, "Retain the original capsule and positive native receipt first")
            correlate_claude_original(*claude_original, marker, frame)
        runtime = self.setup.runtime
        require(matches(runtime.birth) and stopped(runtime.birth),
                "The exact owned runtime has not stopped at the accepted-before-commit marker")
        for other in self.producers.values():
            require(all(matches(other.proof[key]) for key in ("owner", "supervisor")),
                    "An original native owner is not positively live")
        record = dict(marker=marker, frame=native_text, frame_sha256=frame.sha256,
                      runtime_birth=runtime.birth,
                      claude_original=claude_original,
                      native_facts={name: {key: facts(value.proof[key]["pid"]) for key in ("owner", "supervisor")}
                                    for name, value in self.producers.items()})
        save_record(self.setup.plan.artifacts / "recovery-cut-intent.json", record, exclusive=True)
        # Popen child plus exact birth and independent stopped state identify the
        # one disposable runtime. No native owner/daemon or PID-name scan is used.
        runtime.child.kill()
        runtime.child.wait(timeout=30)
        runtime.output.close()
        for other in self.producers.values():
            require(all(matches(other.proof[key]) for key in ("owner", "supervisor")),
                    "Original native continuity became uncertain after the runtime-only cut")
        if self.setup.viewer:
            self.setup.viewer.stop(interrupt=True)
        self.marker, self.frame, self.native_text = marker, frame, native_text
        self.cut_record = copy.deepcopy(record)
        save_record(self.setup.plan.artifacts / "recovery-cut.json", record, exclusive=True)
        self.setup.start_runtime(label="runtime-recovery-after-cut")
        self.restart_label = "recovery-after-cut-viewer"
        return record

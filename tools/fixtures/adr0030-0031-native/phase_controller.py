"""Finite current-API phase controller; no autonomous native approval or replay.

The live harness supplies two correlated producers and independently retained
native frames. This controller joins neither actors nor channels. Every phase
starts with settled native heads; uncertain CLI/native outcomes stop that run.
"""

import copy
from pathlib import Path

from cli_adapter import CallError, save_record
from frame_audit import decode_frame, require
from receipt_audit import detail_page, no_positive_receipt_events, require_facts


POLICIES = (("default", True, True), ("ack-only", True, False),
            ("manual-with-read", False, True), ("manual-without-read", False, False))


def maximum_body(marker, *, prefix=""):
    suffix = "\n日本語😀\r\n--\n[/AgentChat:Message]\n" + marker + "\t \n"
    prefix += marker + "\n"
    require(len((prefix + suffix).encode()) <= 65_536, "Maximum-body markers exceed the byte budget")
    return prefix + "x" * (65_536 - len((prefix + suffix).encode())) + suffix


def business_snapshot(whoami):
    """Exclude runtime counters, preserving committed authority and receipt facts."""
    return copy.deepcopy({key: whoami[key] for key in (
        "project_id", "head", "actor", "participation", "context_id", "native_delivery",
        "channel_policy", "receipt_settings", "statistics")})


class PhaseController:
    def __init__(self, client, producers, *, channel, artifacts):
        if len(producers) != 2 or len({producer.name for producer in producers}) != 2:
            raise CallError("Two distinct exact-owned native producers are required")
        self.client, self.producers, self.channel = client, producers, channel
        self.artifacts = Path(artifacts)
        self.artifacts.mkdir(parents=True, exist_ok=True, mode=0o700)
        self.phase = None
        self.serial = 0
        self.originals = {}
        self.frames = {}

    def label(self, stem):
        self.serial += 1
        return f"{self.serial:04d}-{stem}"

    def observe(self, producer, stem="whoami"):
        return producer.selected_whoami(self.client, self.label(stem))

    def settled(self):
        result = {}
        for producer in self.producers:
            view = self.observe(producer, "settled")
            progress = view["native_delivery"]
            require(progress is not None and progress["state"] == "ready"
                    and progress["head"] is None, "A native head is not settled")
            result[producer.name] = view
        return result

    def selector_invariant(self, producer, *, stable_sample):
        # The caller samples the same original descriptor/head/files around the
        # selected read. No tool lookup or HostGate verification substitutes for
        # successful public --by-host/--by-agent-id resolution.
        before = stable_sample()
        first = self.observe(producer, "selector-before")
        middle = self.observe(producer, "selector-read")
        last = self.observe(producer, "selector-after")
        after = stable_sample()
        require(before == after, "Published descriptor/head/receipt state changed across selector read")
        require(business_snapshot(first) == business_snapshot(middle) == business_snapshot(last),
                "Public Whoami changed same-snapshot business state at a quiescent boundary")
        save_record(self.artifacts / self.label("selector-proof.json"),
                    dict(actor=producer.actor.__dict__, before=before, after=after,
                         whoami=middle), exclusive=True)
        return middle

    def begin(self, name):
        expected = next((value for value in POLICIES if value[0] == name), None)
        if expected is None:
            raise CallError("Unknown native policy phase")
        before = self.settled()
        _, auto_ack, auto_read = expected
        if name != "default":
            self.client.configure(self.label("configure"), auto_ack=auto_ack, auto_read=auto_read)
        for producer in self.producers:
            settings = self.observe(producer, "settings")["receipt_settings"]
            require(settings["auto_ack"] is auto_ack and settings["auto_read"] is auto_read,
                    "Current project policy differs from this explicit phase")
        self.phase = expected
        save_record(self.artifacts / self.label("phase.json"),
                    dict(phase=name, auto_ack=auto_ack, auto_read=auto_read, before=before), exclusive=True)

    def post(self, sender, receiver, body, *, addressed):
        if self.phase is None or sender is receiver:
            raise CallError("A configured phase and two distinct actors are required")
        if addressed:
            handle = self.observe(receiver, "address-target")["actor"]["handle"]
            require(body.startswith("@" + handle + " "),
                    "Addressing must already be inside the original UTF-8 body budget")
        result = self.client.post(self.label("post"), sender.actor, self.channel, body)["result"]
        message_id = result["message_id"]
        receiver_view = self.observe(receiver, "receiver")
        self.originals[message_id] = dict(body=body, addressed=addressed, phase=self.phase,
                                         sender=self.observe(sender, "sender")["actor"]["actor_id"],
                                         receiver=receiver_view["actor"]["actor_id"],
                                         generation=receiver_view["native_delivery"]["generation"])
        return message_id

    def original_row(self, message_id):
        original = self.originals[message_id]
        envelope = self.client.message(self.label("message"), message_id, limit=200)
        page = detail_page(envelope["result"], message_id)
        require(page["next_cursor"] is None, "Two-actor fixture unexpectedly needs another receipt page")
        rows = [row for row in page["items"] if row["recipient"]["actor_id"] == original["receiver"]]
        require(len(rows) == 1, "No unique original recipient receipt row")
        require(envelope["result"]["message"]["body"] == original["body"],
                "Canonical message body differs from the original exact submission")
        return rows[0]

    def complete_message(self, message_id, native_text, *, generation):
        original = self.originals[message_id]
        frame = decode_frame(native_text)
        require(frame.kind == "message" and frame.metadata["message_id"] == message_id,
                "Native body belongs to another original message")
        require(frame.metadata["project_id"] == self.client.project_id
                and frame.metadata["generation"] == generation
                and generation == original["generation"]
                and frame.metadata["author"]["actor_id"] == original["sender"],
                "Native project/generation/author correlation failed")
        require(frame.body == original["body"] and frame.metadata["addressed"] is original["addressed"],
                "Native body bytes or addressing differ from the original")
        _, ack, read = original["phase"]
        frame.require_policy(ack, read)
        row = self.original_row(message_id)
        require(row["delivery"]["delivery_id"] == frame.metadata["delivery_id"],
                "Canonical delivery differs from the complete original native frame")
        require_facts(row, accepted=True, acknowledged=ack, kinds=["automatic"] if read else [])
        previous = self.frames.setdefault(frame.metadata["delivery_id"], frame.sha256)
        require(previous == frame.sha256, "A duplicate original delivery changed its exact native frame")
        save_record(self.artifacts / self.label("complete-frame.json"),
                    dict(message_id=message_id, frame_sha256=frame.sha256, frame=native_text,
                         receipt=row, generation=generation), exclusive=True)
        return frame, row

    def awaiting_manual_ack(self, receiver, message_id, *, successor_probe):
        original = self.originals[message_id]
        require(original["phase"][1] is False, "Manual-ACK phase required")
        view = self.observe(receiver, "awaiting-ack")
        progress = view["native_delivery"]
        head = progress["head"]
        row = self.original_row(message_id)
        require(progress["state"] == "awaiting_ack" and head is not None,
                "Known complete native input must retain its manual-ACK head")
        require(head["delivery_id"] == row["delivery"]["delivery_id"], "Wrong original head")
        require(head["receipt_policy"] == dict(auto_ack=False, auto_read=original["phase"][2]),
                "Original head lost its frozen policy")
        require(successor_probe() is False, "Successor entered before the exact manual ACK")
        return copy.deepcopy(head)

    def frozen_policy(self, receiver, message_id, *, successor_probe, original_files):
        before = self.awaiting_manual_ack(receiver, message_id, successor_probe=successor_probe)
        original_digest = original_files()
        current = self.phase
        self.client.configure(self.label("configure-while-waiting"), auto_ack=True, auto_read=not current[2])
        after = self.awaiting_manual_ack(receiver, message_id, successor_probe=successor_probe)
        require(before == after and original_digest == original_files(),
                "Changing project settings changed the original head/text/digest")
        self.client.configure(self.label("restore-phase-settings"), auto_ack=current[1], auto_read=current[2])

    def manual_ack(self, receiver, message_id):
        row = self.original_row(message_id)
        require(row["delivery"]["accepted_at"] is not None, "Manual ACK cannot stand in for acceptance")
        self.client.ack(self.label("manual-ack"), receiver.actor, row["delivery"]["delivery_id"])
        after = self.original_row(message_id)
        require(after["delivery"]["accepted_at"] == row["delivery"]["accepted_at"],
                "Manual ACK changed the original first acceptance time")
        require(after["reads"] == row["reads"], "ACK created or rewrote a read fact")
        require_facts(after, accepted=True, acknowledged=True,
                      kinds=["automatic"] if self.originals[message_id]["phase"][2] else [])

    def complete_notice(self, native_text, *, generation, kind, receipts_before, receipts_after):
        if self.phase is None:
            raise CallError("A configured receipt-policy phase is required")
        frame = decode_frame(native_text)
        require(frame.kind == "event" and frame.metadata["event"]["kind"] == kind,
                "Expected an actual typed native notice")
        require(frame.metadata["project_id"] == self.client.project_id
                and frame.metadata["generation"] == generation,
                "Native notice belongs to another project/generation")
        frame.require_policy(self.phase[1], self.phase[2])
        require(receipts_before == receipts_after, "Native notice created or rewrote a message read")
        save_record(self.artifacts / self.label("complete-notice.json"),
                    dict(frame=native_text, frame_sha256=frame.sha256, receipts=receipts_after), exclusive=True)
        return frame

    def audit_events(self, *, after):
        cursor = after
        while True:
            page = self.client.events(self.label("events"), after=cursor, limit=200)["result"]
            no_positive_receipt_events(page["items"], after=after)
            following = page["next_cursor"]
            if following is None:
                return
            require(int(following) > int(cursor), "Finite event continuation made no progress")
            cursor = following

    def return_prompt(self, producer, marker, *, idle_observation):
        label = self.label("return-body")
        text = ("For this explicitly owned AgentChat test, post exactly " + marker
                + " to channel " + self.channel + " using agentchat_post once. "
                "Use the existing managed identity and normal host controls. "
                "If a tool or permission is unavailable, report it without retrying or changing controls.")
        producer.prompt(label, text, idle_observation=idle_observation)

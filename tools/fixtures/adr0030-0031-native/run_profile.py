"""Run one explicit current-image profile in the single integrated campaign.

Invoke only after the integration source/image freeze. Commands are finite
test-controller artifacts, not AgentChat task tracking. Native decisions require
the exact concrete terminal digest and an explicit human response; no login,
trust, permission or tool approval is answered automatically.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import time

from actor_producer import Image, sha256
from cli_adapter import CallError, save_record
from fixture_plan import FixturePlan
from fixture_setup import FixtureSetup
from frame_audit import decode_frame, require
from native_observer import NativeObserver, user_texts
from phase_controller import PhaseController
import phase_commands
from receipt_audit import detail_page
from recovery_controller import RecoveryController, recovered_facts


def load_plan(document):
    required = {"host", "case", "images", "native_version", "codex_socket"}
    if not isinstance(document, dict) or set(document) != required:
        raise CallError("The exact explicit profile input schema is required")
    images = document["images"]
    if set(images) != {"cli", "helper", "native", "source_seal"}:
        raise CallError("All four frozen image/source inputs are required")
    selected = {}
    for name, value in images.items():
        if set(value) != {"path", "sha256"}:
            raise CallError("Each current input requires an exact path and SHA-256")
        selected[name] = Image(Path(value["path"]), value["sha256"])
    if document["host"] == "codex" and not document["codex_socket"]:
        raise CallError("The existing inspected Codex daemon socket is required")
    return FixturePlan(host=document["host"], case=Path(document["case"]),
                       native_version=document["native_version"], **selected)


class ProfileCampaign:
    def __init__(self, plan, *, socket_path=None):
        self.setup = FixtureSetup(plan)
        self.socket_path = socket_path
        self.actors = {}
        self.observers = {}
        self.controller = None
        self.recovery = None
        self.serial = 0
        self.stage = "unprepared"
        self.stop_requested = False

    def label(self, stem):
        self.serial += 1
        return f"profile-{self.serial:06d}-{stem}"

    def prepare(self):
        self.setup.prepare()
        for name in ("commands", "results"):
            (self.setup.plan.case / name).mkdir(mode=0o700)
        self.setup.start_runtime()
        self.stage = "runtime-starting"

    def advance_startup(self):
        if self.stage == "runtime-starting":
            if not self.setup.poll_runtime_ready():
                return
            self.setup.start_setup_observer()
            self.actors["receiver"] = self.setup.start_producer("receipt-receiver")
            self.stage = "receiver-starting"
        if self.stage == "receiver-starting" and self.actors["receiver"].readiness():
            self.actors["sender"] = self.setup.start_producer("receipt-sender")
            self.stage = "sender-starting"
        if self.stage == "sender-starting" and self.actors["sender"].readiness():
            if not all(producer.activation_reported() for producer in self.actors.values()):
                return
            for name, producer in self.actors.items():
                producer.selected_whoami(self.setup.client, self.label("first-selector-" + name))
                self.observers[name] = NativeObserver(producer,
                    artifacts=self.setup.plan.artifacts / (name + "-native"), socket_path=self.socket_path)
            self.controller = PhaseController(self.setup.client, list(self.actors.values()),
                channel="native", artifacts=self.setup.plan.artifacts / "phases")
            self.stage = "ready"
            save_record(self.setup.plan.artifacts / "profile-ready.json", self.inspect(), exclusive=True)

    def inspect(self):
        return dict(stage=self.stage, host=self.setup.plan.host, case=str(self.setup.plan.case),
                    project_id=None if not self.setup.client else self.setup.client.project_id,
                    runtime=None if not self.setup.runtime else self.setup.runtime.birth,
                    actors={name: dict(root=None if not value.actor else value.actor.chat_id,
                                       context=value.context, proof=value.proof,
                                       supervisor=value.terminal.birth,
                                       exit_code=value.terminal_status(),
                                       terminal=str(value.terminal.raw),
                                       terminal_sha256=sha256(value.terminal.raw))
                            for name, value in self.actors.items()})

    def dispatch(self, command):
        kind = command["kind"]
        if kind == "inspect":
            return self.inspect()
        if kind == "native-decision":
            actor = self.actors[command["actor"]]
            actor.terminal.decision(label=self.label("concrete-native-decision"),
                terminal_sha256=command["terminal_sha256"], key=command["key"],
                authorization=command["authorization"])
            return dict(input_sent=True, task_or_native_success=False)
        if kind == "cosmetic-theme-default":
            actor = self.actors[command["actor"]]
            if actor.host != "claude":
                raise CallError("The initial cosmetic theme path is Claude-only")
            actor.terminal.cosmetic_theme(label=self.label("cosmetic-theme-default"),
                                         terminal_sha256=command["terminal_sha256"])
            return dict(cosmetic_default_confirmed=True, task_or_native_success=False)
        if kind == "stop-owned-runtime":
            if any(value.terminal.child.poll() is None for value in self.actors.values()):
                raise CallError("Finish the two owned native launches normally before runtime cleanup")
            if self.setup.viewer:
                self.setup.viewer.stop(interrupt=True)
            self.setup.runtime.stop()
            self.stop_requested = True
            return dict(only_owned_runtime_and_viewer_stopped=True)
        if kind == "finish-native":
            name = command["actor"]
            producer = self.actors[name]
            producer.selected_whoami(self.setup.client, self.label("before-normal-exit"))
            observer = self.observers.get(name) or NativeObserver(producer,
                artifacts=self.setup.plan.artifacts / (name + "-native"), socket_path=self.socket_path)
            observation = observer.observe(self.label("normal-exit-idle"))
            producer.finish(self.label("normal-owned-exit"), idle_observation=observation)
            return dict(normal_native_exit_requested=True, exited=False)
        if kind == "recovery-ready":
            if self.recovery is None or not self.recovery.ready():
                return dict(runtime_ready=False)
            self.stage = "ready"
            return dict(runtime_ready=True)
        if self.stage != "ready":
            raise CallError("Native startup is still pending; inspect the original terminal")
        if kind == "begin":
            self.controller.begin(command["phase"])
            return dict(phase=command["phase"])
        if kind == "check-selector":
            return phase_commands.selector(self, command["actor"])
        if kind in ("check-manual-fence", "frozen-policy"):
            return phase_commands.manual_fence(self, command, change_settings=kind == "frozen-policy")
        if kind == "check-notice":
            return phase_commands.notice(self, command)
        if kind == "reserve":
            return self.setup.client.reserve(self.label("mixed-notice"), self.actors[command["actor"]].actor,
                                             command["patterns"])["result"]
        if kind == "release":
            return self.setup.client.release(self.label("mixed-notice-release"), self.actors[command["actor"]].actor,
                                             command["reservation_id"], command["generation"])["result"]
        if kind == "recovery-arm":
            if self.recovery is not None:
                raise CallError("This original cut is already armed or consumed")
            self.recovery = RecoveryController(self.setup, self.controller, self.actors, self.observers)
            self.recovery.arm()
            self.stage = "recovery-runtime-starting"
            return dict(marker=str(self.recovery.marker_path), runtime_ready=False)
        if kind == "recovery-cut":
            if self.recovery is None:
                raise CallError("Original cut is not armed")
            name = command["actor"]
            marker = json.loads(self.recovery.marker_path.read_text())
            frames = self.native_frames(name)
            matching = [frame for frame in frames if frame.kind == "message"
                        and frame.metadata["message_id"] == marker["message_id"]]
            require(len(matching) == 1, "Original accepted native body is absent or duplicated")
            native_text = next(text for text in self.native_texts[name]
                               if hashlib.sha256(text.encode()).hexdigest() == matching[0].sha256)
            original = None
            if self.setup.plan.host == "claude":
                observation = self.observers[name].observe(self.label("original-receipt-namespace"))
                saved = self.observers[name].claude_original_files(self.label("original-receipt-files"),
                    observation["raw"]["directory"])
                files = [value["value"] for key, value in saved.items()
                         if key.startswith("input-") and key != "input-head.json"
                         and value["value"]["token"] == marker["token"]]
                receipts = [value["value"] for key, value in saved.items()
                            if key.startswith("accepted-input-") and value["value"]["token"] == marker["token"]]
                require(len(files) == 1 and len(receipts) == 1, "No unique positive original capsule/receipt")
                rows = self.observers[name].claude_rows(self.label("original-native-id"))
                require(any(row.get("type") == "user" and row.get("uuid") == receipts[0]["native_id"]
                            for row in rows), "Original receipt native UUID is absent from the actual transcript")
                original = files[0], receipts[0]
            result = self.recovery.cut(name, native_text, claude_original=original)
            self.stage = "recovery-runtime-starting"
            return result
        if kind == "recovery-check":
            if self.recovery is None or self.recovery.cut_record is None:
                raise CallError("No original runtime cut was retained")
            name = command["actor"]
            marker, frame = self.recovery.marker, self.recovery.frame
            native_frames = self.native_frames(name)
            count = sum(value.kind == "message" and value.metadata["delivery_id"] == marker["delivery_id"]
                        for value in native_frames)
            result = self.setup.client.message(self.label("recovered-message"), marker["message_id"], limit=200)["result"]
            page = detail_page(result, marker["message_id"])
            view = self.actors[name].selected_whoami(self.setup.client, self.label("recovered-whoami"))
            rows = [row for row in page["items"] if row["recipient"]["actor_id"] == view["actor"]["actor_id"]]
            require(len(rows) == 1 and page["next_cursor"] is None, "No unique original recovered receipt")
            recovered_facts(self.setup.plan.host, marker, frame, rows[0], view["native_delivery"],
                            original_native_count=count)
            return dict(receipt=rows[0], progress=view["native_delivery"], original_native_count=count)
        if kind == "post":
            message_id = self.controller.post(self.actors[command["sender"]],
                self.actors[command["receiver"]], command["body"], addressed=command["addressed"])
            return dict(message_id=message_id, complete_native_acceptance=False)
        if kind == "manual-ack":
            self.controller.manual_ack(self.actors[command["actor"]], command["message_id"])
            return dict(ack_returned=True)
        if kind == "check-message":
            name = command["actor"]
            frames = self.native_frames(name)
            matching = [frame for frame in frames if frame.kind == "message"
                        and frame.metadata["message_id"] == command["message_id"]]
            require(len(matching) == 1, "Original native body is absent or duplicated")
            view = self.actors[name].selected_whoami(self.setup.client, self.label("check-message-whoami"))
            text = next(text for text in self.native_texts[name]
                        if hashlib.sha256(text.encode()).hexdigest() == matching[0].sha256)
            frame, row = self.controller.complete_message(command["message_id"], text,
                generation=view["native_delivery"]["generation"])
            return dict(frame_sha256=frame.sha256, receipt=row)
        if kind == "check-fifo":
            frames = self.native_frames(command["actor"])
            ids = [frame.metadata["message_id"] if frame.kind == "message" else frame.metadata["event"]["event_id"]
                   for frame in frames]
            expected = command["ids"]
            selected = [value for value in ids if value in expected]
            require(selected == expected, "Native complete-input order differs or contains duplicates")
            return dict(actual_order=selected)
        if kind == "prompt":
            observer = self.observers[command["actor"]]
            observation = observer.observe(self.label("before-prompt"))
            self.actors[command["actor"]].prompt(self.label("model-prompt"), command["text"],
                                                idle_observation=observation)
            return dict(submitted=True, completed=False)
        if kind == "snapshot":
            result = self.inspect()
            for name, observer in self.observers.items():
                result["actors"][name]["whoami"] = self.actors[name].selected_whoami(
                    self.setup.client, self.label("snapshot-whoami"))
                result["actors"][name]["native"] = observer.observe(self.label("snapshot-native"))
                if self.setup.plan.host == "claude":
                    result["actors"][name]["rows"] = observer.claude_rows(self.label("transcript"))
            return result
        if kind == "audit-events":
            self.controller.audit_events(after=command["after"])
            return dict(no_new_positive_receipt_events=True)
        raise CallError("Unknown explicit campaign command")

    def native_frames(self, name):
        observation = self.observers[name].observe(self.label("frame-observation"))
        native = observation["raw"] if self.setup.plan.host == "codex" else self.observers[name].claude_rows(
            self.label("frame-transcript"))
        texts = [text for text in user_texts(self.setup.plan.host, native)
                 if text.startswith(("[AgentChat:Message]\n", "[AgentChat:Event]\n"))]
        if not hasattr(self, "native_texts"):
            self.native_texts = {}
        self.native_texts[name] = texts
        return phase_commands.ordered_frames(texts)

    def commands(self):
        root = self.setup.plan.case
        for path in sorted((root / "commands").glob("*.json")):
            if path.is_symlink() or path.stat().st_uid != os.geteuid() or path.stat().st_size > 1_048_576:
                raise CallError("Unexpected controller command ownership/type/size")
            result_path = root / "results" / path.name
            if result_path.exists():
                continue
            raw = path.read_bytes()
            save_record(result_path, dict(state="intent", command_sha256=hashlib.sha256(raw).hexdigest(),
                                          outcome_unknown=True), exclusive=True)
            try:
                result = self.dispatch(json.loads(raw))
                record = dict(state="returned", command_sha256=hashlib.sha256(raw).hexdigest(), result=result)
            except Exception as error:
                # No exception requeues a semantic operation. Original CLI UUIDs
                # remain in their separate captured intents for read-only inquiry.
                record = dict(state="failed", command_sha256=hashlib.sha256(raw).hexdigest(),
                              error=type(error).__name__ + ": " + str(error), outcome_unknown=True)
            save_record(result_path, record)


def drive(campaign):
    campaign.prepare()
    print(json.dumps(dict(case=str(campaign.setup.plan.case), stage=campaign.stage)), flush=True)
    startup_failed = False
    while not campaign.stop_requested:
        if not startup_failed and campaign.stage != "ready":
            try:
                campaign.advance_startup()
            except Exception as error:
                save_record(campaign.setup.plan.artifacts / "startup-failed.json",
                    dict(error=type(error).__name__ + ": " + str(error), snapshot=campaign.inspect()), exclusive=True)
                startup_failed = True
        campaign.commands()
        time.sleep(.1)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inputs", type=Path, required=True)
    arguments = parser.parse_args()
    document = json.loads(arguments.inputs.read_text())
    drive(ProfileCampaign(load_plan(document), socket_path=document["codex_socket"]))


if __name__ == "__main__":
    main()

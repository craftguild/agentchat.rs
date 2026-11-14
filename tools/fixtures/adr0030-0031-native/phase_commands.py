"""Live phase commands sample only the exact producer's retained native state."""

import hashlib
import json

from cli_adapter import CallError
from frame_audit import decode_frame, require


def ordered_frames(texts):
    frames = [decode_frame(text) for text in texts]
    previous = {}
    for frame in frames:
        metadata = frame.metadata
        key = metadata["project_id"], metadata["generation"]
        sequence = int(metadata["seq"] if frame.kind == "message" else metadata["event"]["seq"])
        require(sequence > previous.get(key, -1), "Native original inputs repeated or overtook project order")
        previous[key] = sequence
    return frames


def original_files(namespace, context_id):
    values = {}
    for pattern in ("input-*.json", "accepted-input-*.json"):
        for path in namespace.glob(pattern):
            if path.is_symlink():
                raise CallError("Original capsule/receipt became a symlink")
            try:
                raw = path.read_bytes()
            except FileNotFoundError:
                continue
            value = json.loads(raw)
            if value.get("context_id") == context_id:
                values[path.name] = hashlib.sha256(raw).hexdigest()
    return values


def selector(campaign, name):
    from pathlib import Path
    producer = campaign.actors[name]
    # A legitimate reattach can replace the setup descriptor before this test.
    # Resolve it through the existing strict public/report/physical proof checks
    # before fixing the baseline; changes inside the measured boundary still fail.
    producer.selected_whoami(campaign.setup.client, campaign.label("selector-synchronize"))
    namespace = None
    if producer.host == "claude":
        observed = campaign.observers[name].observe(campaign.label("selector-idle"))
        require(observed["phase"] == "idle", "Native must be idle for a selector invariance boundary")
        namespace = Path(observed["raw"]["directory"])
    def sample():
        require(producer.readiness(), "Original selected route is not ready")
        return dict(context=producer.context, proof=producer.proof,
                    published_ready_sha256=hashlib.sha256(producer.ready_path.read_bytes()).hexdigest(),
                    originals={} if namespace is None else original_files(namespace, producer.context["context_id"]))
    return campaign.controller.selector_invariant(producer, stable_sample=sample)


def manual_fence(campaign, command, *, change_settings):
    from pathlib import Path
    name = command["actor"]
    producer = campaign.actors[name]
    successors = command["successor_message_ids"]
    require(isinstance(successors, list) and bool(successors), "Name the actual queued successors")
    def entered():
        return any(frame.kind == "message" and frame.metadata["message_id"] in successors
                   for frame in campaign.native_frames(name))
    if not change_settings:
        return campaign.controller.awaiting_manual_ack(producer, command["message_id"], successor_probe=entered)
    namespace = None
    if producer.host == "claude":
        observed = campaign.observers[name].observe(campaign.label("frozen-original-namespace"))
        namespace = Path(observed["raw"]["directory"])
    def sample():
        if namespace is None:
            return {delivery: digest for delivery, digest in campaign.controller.frames.items()}
        return original_files(namespace, producer.context["context_id"])
    campaign.controller.frozen_policy(producer, command["message_id"], successor_probe=entered, original_files=sample)
    return dict(original_policy_and_successor_fence_unchanged=True)


def notice(campaign, command):
    name = command["actor"]
    texts = campaign.native_frames(name)
    matches = [frame for frame in texts if frame.kind == "event"
               and frame.metadata["event"]["event_id"] == command["event_id"]]
    require(len(matches) == 1, "Original typed notice is absent or duplicated")
    frame = matches[0]
    text = next(text for text in campaign.native_texts[name]
                if hashlib.sha256(text.encode()).hexdigest() == frame.sha256)
    view = campaign.actors[name].selected_whoami(campaign.setup.client, campaign.label("notice-whoami"))
    campaign.controller.complete_notice(text, generation=view["native_delivery"]["generation"],
        kind=command["event_kind"], receipts_before=command["receipts_before"],
        receipts_after={message: campaign.controller.original_row(message) for message in command["receipts_before"]})
    return dict(frame_sha256=frame.sha256)

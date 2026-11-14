"""Explicit public CLI calls for an owned fixture, without native authority guesses.

The owner supplies current native IDs from independent readiness evidence. This
adapter does not launch hosts, create identities, retry unknown mutations or
change any inherited native trust, sandbox or approval control.
"""

from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import subprocess
import uuid


class CallError(RuntimeError):
    """A captured CLI operation failed; its original outcome is retained."""


def save_record(path, record, *, exclusive=False):
    mode = "x" if exclusive else "w"
    with path.open(mode) as output:
        os.chmod(path, 0o600)
        json.dump(record, output, indent=2)
        output.write("\n")
        output.flush()
        os.fsync(output.fileno())
    directory = os.open(path.parent, os.O_RDONLY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


@dataclass(frozen=True)
class NativeActor:
    host: str
    chat_id: str

    def arguments(self):
        if self.host not in ("codex", "claude") or not self.chat_id:
            raise CallError("Exact host/native-chat evidence is required")
        return ["--by-host", self.host, "--by-agent-id", self.chat_id]


class FixtureCli:
    """Transport only; actor, receipt and delivery policy remains in the product."""

    def __init__(self, *, image, image_sha256, project_id, project, state, config,
                 artifacts, profile=None, runner=None):
        self.image = Path(image)
        self.image_sha256 = image_sha256
        self.project_id = project_id
        self.project = Path(project)
        self.artifacts = Path(artifacts)
        self.artifacts.mkdir(parents=True, exist_ok=True, mode=0o700)
        self.environment = os.environ.copy()
        self.environment.update(AGENTCHAT_HOME=str(state), AGENTCHAT_USER_CONFIG_ROOT=str(config))
        self.environment.pop("AGENTCHAT_CONTEXT", None)
        if profile is not None:
            self.environment["CLAUDE_CONFIG_DIR"] = str(profile)
        self.runner = runner or subprocess.run

    def _call(self, label, arguments, *, actor=None, body=None, request_id=None):
        if not label or any(char not in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_" for char in label):
            raise CallError("An exact artifact label is required")
        prefix = self.artifacts / label
        if prefix.with_suffix(".json").exists():
            raise CallError("A completed or uncertain operation label cannot be replayed")
        digest = hashlib.sha256()
        with self.image.open("rb") as source:
            for block in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(block)
        if digest.hexdigest() != self.image_sha256:
            raise CallError("The selected fixture image changed before this call")
        command = [str(self.image), "--project", str(self.project), "--json"]
        if request_id is not None:
            command += ["--request-id", request_id]
        if actor is not None:
            command += actor.arguments()
        command += arguments
        record = {"command": command, "request_id": request_id,
                  "project_id": self.project_id, "state": "intent",
                  "outcome_unknown": True, "exit_code": None,
                  "body_sha256": None if body is None else hashlib.sha256(body.encode()).hexdigest()}
        # Persist the original semantic operation before spawning. A controller
        # crash cannot erase its UUID and silently allow a duplicate submission.
        save_record(prefix.with_suffix(".json"), record, exclusive=True)
        try:
            result = self.runner(command, cwd=self.project, env=self.environment,
                                 input=None if body is None else body.encode("utf-8"),
                                 capture_output=True, timeout=60, check=False)
            stdout, stderr = result.stdout, result.stderr
            record["exit_code"] = result.returncode
            record["state"] = "returned"
        except subprocess.TimeoutExpired as error:
            stdout, stderr = error.stdout or b"", error.stderr or b""
            record["outcome_unknown"] = True
        except OSError as error:
            stdout, stderr = b"", str(error).encode()
            record["spawn_error"] = type(error).__name__
            record["outcome_unknown"] = False
            record["state"] = "spawn_error"
        prefix.with_suffix(".stdout").write_bytes(stdout)
        prefix.with_suffix(".stderr").write_bytes(stderr)
        save_record(prefix.with_suffix(".json"), record)
        if "spawn_error" in record:
            raise CallError("CLI did not start; inspect the captured spawn error")
        if record["state"] == "intent":
            raise CallError("CLI outcome is unknown; inspect the original request without resubmission")
        if record["exit_code"]:
            raise CallError(f"CLI operation failed; inspect {prefix}.json")
        try:
            envelope = json.loads(stdout)
        except (ValueError, TypeError) as error:
            raise CallError("CLI returned no complete machine envelope") from error
        if (not isinstance(envelope, dict) or type(envelope.get("version")) is not int
                or envelope["version"] != 1 or "result" not in envelope or "error" in envelope):
            raise CallError("CLI did not return a successful current envelope")
        if envelope.get("project_id") != self.project_id:
            raise CallError("CLI envelope belongs to a different fixture project")
        if request_id is not None and envelope.get("request_id") != request_id:
            raise CallError("CLI envelope belongs to a different semantic operation")
        record.update(state="success", outcome_unknown=False)
        save_record(prefix.with_suffix(".json"), record)
        return envelope

    def whoami(self, label, actor):
        if not isinstance(actor, NativeActor):
            raise CallError("Whoami cannot fall back to an observer")
        return self._call(label, ["whoami"], actor=actor)

    def message(self, label, message_id, *, after="0", limit=50):
        # Deliberately context-free human observation, distinct from selected
        # native actor reads. Retrieval cannot create ACK or read facts.
        return self._call(label, ["message", message_id, "--after", after, "--limit", str(limit)])

    def events(self, label, *, after="0", limit=50):
        return self._call(label, ["events", "--after", after, "--limit", str(limit)])

    def post(self, label, actor, channel, body, *, request_id=None):
        if not isinstance(actor, NativeActor):
            raise CallError("Posting requires an exact native actor")
        return self._call(label, ["post", "--channel", channel, "--stdin"], actor=actor,
                          body=body, request_id=request_id or str(uuid.uuid4()))

    def ack(self, label, actor, delivery_id, *, request_id=None):
        if not isinstance(actor, NativeActor):
            raise CallError("ACK requires an exact native actor")
        return self._call(label, ["ack", delivery_id], actor=actor,
                          request_id=request_id or str(uuid.uuid4()))

    def configure(self, label, *, auto_ack, auto_read, request_id=None):
        if type(auto_ack) is not bool or type(auto_read) is not bool:
            raise CallError("Receipt settings require explicit booleans")
        return self._call(label, ["install", "--scope", "directory",
                                 "--auto-ack", str(auto_ack).lower(),
                                 "--auto-read", str(auto_read).lower()],
                          request_id=request_id or str(uuid.uuid4()))

    def reserve(self, label, actor, patterns, *, request_id=None):
        if not isinstance(actor, NativeActor) or not patterns:
            raise CallError("Reservation notice production requires an exact actor and paths")
        return self._call(label, ["reserve", *patterns, "--reason", "owned native matrix notice"],
                          actor=actor, request_id=request_id or str(uuid.uuid4()))

    def release(self, label, actor, reservation_id, generation, *, request_id=None):
        if not isinstance(actor, NativeActor):
            raise CallError("Reservation release requires the exact fixture owner")
        return self._call(label, ["reservations", "release", reservation_id,
                                 "--generation", str(generation)], actor=actor,
                          request_id=request_id or str(uuid.uuid4()))

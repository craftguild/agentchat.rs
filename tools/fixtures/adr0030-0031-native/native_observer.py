"""Retain actual owned-root host observations without changing native controls."""

import json
import os
from pathlib import Path
import stat
import subprocess
import time

from actor_producer import sha256
from cli_adapter import CallError, save_record
from native_transport import Codex
from owner_facts import matches


def rollout_bytes(path):
    """Bound this observer's retention, never native traffic or message size."""
    maximum = 16 * 1024 * 1024
    before = path.lstat()
    if (not stat.S_ISREG(before.st_mode) or before.st_uid != os.geteuid()
            or before.st_size > maximum):
        raise CallError("Native rollout ownership/type/observation bound changed")
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        opened = os.fstat(descriptor)
        if (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino):
            raise CallError("Native rollout changed during opening")
        with os.fdopen(descriptor, "rb", closefd=False) as source:
            raw = source.read(maximum + 1)
    finally:
        os.close(descriptor)
    after = path.lstat()
    if (len(raw) > maximum or any(getattr(before, key) != getattr(after, key)
            for key in ("st_dev", "st_ino", "st_mtime_ns", "st_size"))):
        raise CallError("Native rollout changed during bounded observation")
    return raw, before


class NativeObserver:
    def __init__(self, producer, *, artifacts, socket_path=None, runner=subprocess.run):
        self.producer = producer
        self.artifacts = Path(artifacts)
        self.artifacts.mkdir(parents=True, exist_ok=True, mode=0o700)
        self.socket_path = socket_path
        self.runner = runner

    def observe(self, label):
        producer = self.producer
        if not producer.readiness():
            raise CallError("Native observation has no correlated owned readiness")
        if producer.host == "codex":
            if not self.socket_path:
                raise CallError("The existing daemon's exact inspected socket is required")
            client = Codex(self.socket_path)
            try:
                raw = client.request("thread/read", dict(threadId=producer.actor.chat_id, includeTurns=True))
                pending = [value for value in client.observed if value.get("id") is not None
                           and value.get("params", {}).get("threadId") == producer.actor.chat_id]
            finally:
                client.close()
            thread = raw["thread"]
            if thread["id"] != producer.actor.chat_id:
                raise CallError("Native read returned another root")
            phase = thread["status"]["type"]
        else:
            producer.helper.check()
            command = [str(producer.helper.path), "observe", str(producer.state),
                       str(producer.native.path), str(producer.proof["owner"]["pid"])]
            response = self.runner(command, cwd=producer.project, env=producer.environment,
                                   capture_output=True, timeout=15, check=False)
            (self.artifacts / (label + ".stdout")).write_bytes(response.stdout)
            (self.artifacts / (label + ".stderr")).write_bytes(response.stderr)
            if response.returncode:
                raise CallError("Original Claude observation failed; retain the native outcome")
            raw = json.loads(response.stdout)
            lifecycle = raw["registration"]["lifecycle"]
            context = lifecycle["registration"]["context"]
            if (raw["owner"] != producer.proof["owner"] or lifecycle["stage"] != "project"
                    or context != producer.context):
                raise CallError("Claude observation changed the exact original owner/root/descriptor")
            phase, pending = raw["phase"], []
        if not all(matches(producer.proof[key]) for key in ("owner", "supervisor")):
            raise CallError("Native owner continuity changed during observation")
        observation = dict(root=producer.actor.chat_id, context_id=producer.context["context_id"],
                           owner=producer.proof["owner"], phase=phase, pending_requests=pending,
                           observed_monotonic_ns=time.monotonic_ns(), raw=raw)
        save_record(self.artifacts / (label + ".json"), observation, exclusive=True)
        return observation

    def codex_rows(self, label, observation, *, client):
        """Read only the exact native API path, including passive injected items."""
        producer = self.producer
        if (producer.host != "codex" or not producer.readiness()
                or observation["root"] != producer.actor.chat_id
                or observation["context_id"] != producer.context["context_id"]
                or observation["owner"] != producer.proof["owner"]):
            raise CallError("Native rollout has no current exact owned observation")
        selected = producer.selected_whoami(client, label + "-selector-before")
        if selected["context_id"] != observation["context_id"]:
            raise CallError("Current public descriptor differs from the native observation")
        thread = observation["raw"]["thread"]
        path = Path(thread["path"])
        # The API supplies the complete path. This format bound does not infer
        # a root from its name, search directories or fall back to other files.
        if (thread["id"] != producer.actor.chat_id or not path.is_absolute()
                or not path.name.startswith("rollout-") or path.suffix != ".jsonl"):
            raise CallError("Native API returned no exact rollout path/root")
        raw, metadata = rollout_bytes(path)
        rows = [json.loads(line) for line in raw.splitlines() if line]
        sessions = [row["payload"]["id"] for row in rows if row.get("type") == "session_meta"]
        if (sessions != [producer.actor.chat_id]
                or not all(matches(producer.proof[key]) for key in ("owner", "supervisor"))):
            raise CallError("Native rollout session or original physical lifetime changed")
        after = producer.selected_whoami(client, label + "-selector-after")
        if after["context_id"] != selected["context_id"]:
            raise CallError("Current public descriptor changed during rollout observation")
        destination = self.artifacts / (label + ".jsonl")
        with destination.open("xb") as output:
            os.chmod(destination, 0o600)
            output.write(raw)
        save_record(self.artifacts / (label + "-source.json"),
                    dict(native_api_path=str(path), root=producer.actor.chat_id,
                         context_id=producer.context["context_id"], proof=producer.proof,
                         device=metadata.st_dev, inode=metadata.st_ino, bytes=len(raw),
                         sha256=sha256(destination), source="native response_item JSONL"), exclusive=True)
        return rows

    def claude_rows(self, label):
        producer = self.producer
        if producer.host != "claude" or not producer.readiness():
            raise CallError("Exact Claude readiness is required for transcript observation")
        paths = list((Path(producer.profile) / "projects").rglob(producer.actor.chat_id + ".jsonl"))
        if len(paths) != 1 or paths[0].is_symlink():
            raise CallError("No unique original Claude transcript")
        raw = paths[0].read_bytes()
        destination = self.artifacts / (label + ".jsonl")
        with destination.open("xb") as output:
            output.write(raw)
        return [json.loads(line) for line in raw.splitlines() if line]

    def claude_original_files(self, label, namespace):
        """Capture only the exact owned namespace selected by the host probe.

        These original input/receipt bytes still require their complete product
        token/digest/owner/root/generation validation; file presence is no receipt.
        """
        producer = self.producer
        namespace = Path(namespace)
        if producer.host != "claude" or not producer.readiness() or namespace.is_symlink():
            raise CallError("No exact Claude namespace observation")
        if producer.state.resolve() not in namespace.resolve().parents:
            raise CallError("Native namespace is outside this explicit fixture state")
        values = {}
        destination = self.artifacts / label
        destination.mkdir(mode=0o700, exist_ok=False)
        for pattern in ("input-*.json", "accepted-input-*.json"):
            for path in namespace.glob(pattern):
                if path.is_symlink():
                    continue
                try:
                    raw = path.read_bytes()
                    value = json.loads(raw)
                except FileNotFoundError:
                    continue
                if value.get("context_id") != producer.context["context_id"] or not value.get("token"):
                    continue
                with (destination / path.name).open("xb") as output:
                    output.write(raw)
                values[path.name] = dict(sha256=sha256(destination / path.name), value=value)
        save_record(destination / "manifest.json", values, exclusive=True)
        return values


def user_texts(host, native):
    """Extract exact native user text, never previews or assistant reconstructions."""
    texts = []
    if host == "codex":
        messages = [item for turn in native["thread"]["turns"] for item in turn["items"]
                    if item["type"] == "userMessage"]
        content = [item["content"] for item in messages]
    elif host == "claude":
        content = [row["message"]["content"] for row in native
                   if row.get("type") == "user" and "message" in row]
    else:
        raise CallError("Unknown native transcript host")
    for blocks in content:
        if isinstance(blocks, str):
            texts.append(blocks)
        elif isinstance(blocks, list):
            texts.extend(block["text"] for block in blocks
                         if block.get("type") == "text" and isinstance(block.get("text"), str))
    return texts


def codex_rollout_texts(rows):
    """Original user input text only, separate from ordinary turn projections."""
    return [block["text"] for row in rows if row.get("type") == "response_item"
            and row.get("payload", {}).get("type") == "message" and row["payload"].get("role") == "user"
            for block in row["payload"]["content"]
            if block.get("type") == "input_text" and isinstance(block.get("text"), str)]

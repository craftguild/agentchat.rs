#!/usr/bin/env python3
"""Shared process and observation helpers for the Claude body campaign."""

import json
import os
from pathlib import Path
import subprocess
import sys
import time


class CampaignContext:
    """Own the disposable fixture process and its command/result protocol."""

    def __init__(self, config: Path, rig: Path):
        values = json.loads(config.read_text())
        overrides = {
            "case": os.environ.get("AGENTCHAT_BODY_CASE"),
            "A": os.environ.get("AGENTCHAT_BODY_AGENTCHAT"),
            "A_sha256": os.environ.get("AGENTCHAT_BODY_AGENTCHAT_SHA256"),
            "helper_A": os.environ.get("AGENTCHAT_BODY_NATIVE_HELPER"),
            "helper_A_sha256": os.environ.get(
                "AGENTCHAT_BODY_NATIVE_HELPER_SHA256"
            ),
            "helper_B": os.environ.get("AGENTCHAT_BODY_NATIVE_HELPER"),
            "helper_B_sha256": os.environ.get(
                "AGENTCHAT_BODY_NATIVE_HELPER_SHA256"
            ),
        }
        values.update({key: value for key, value in overrides.items() if value})
        case = Path(values["case"])
        case.parent.mkdir(parents=True, exist_ok=True)
        if any(overrides.values()):
            config = case.parent / f"{case.name}.effective-config.json"
            config.write_text(json.dumps(values, indent=2) + "\n")
        self.values = values
        self.case = case
        self.counter = 0
        log = case.with_suffix(".body-driver.log")
        with log.open("wb") as output:
            self.process = subprocess.Popen(
                [sys.executable, str(rig), str(config)],
                stdout=output,
                stderr=subprocess.STDOUT,
                env=os.environ.copy(),
            )

    def wait_path(self, path: Path, timeout: float = 240.0):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if path.exists():
                return path
            if self.process.poll() is not None:
                raise RuntimeError(
                    f"rig exited {self.process.returncode} before {path.name}"
                )
            time.sleep(0.1)
        raise TimeoutError(str(path))

    def issue(self, label: str, value: dict, timeout: float = 240.0):
        command = self.case / "commands" / f"{label}.json"
        result = self.case / "results" / f"{label}.json"
        temp = command.with_suffix(".tmp")
        temp.write_text(json.dumps(value, indent=2) + "\n")
        temp.replace(command)
        parsed = json.loads(self.wait_path(result, timeout).read_text())
        if parsed.get("fixture_command_error"):
            raise RuntimeError(f"{label}: {parsed['fixture_command_error']}")
        return parsed

    def direct_env(self):
        result = os.environ.copy()
        result.update(
            AGENTCHAT_HOME=str(self.case / "state"),
            AGENTCHAT_USER_CONFIG_ROOT=str(
                Path(self.values["profile"]).resolve().parent
            ),
            CLAUDE_CONFIG_DIR=str(Path(self.values["profile"]).resolve()),
            TERM="xterm-256color",
            DISABLE_AUTOUPDATER="1",
        )
        result.pop("AGENTCHAT_CONTEXT", None)
        return result

    def cli(self, label: str, arguments: list[str], context=None, body=None):
        command = [
            str(self.case / "prefix" / "agentchat"),
            "--project",
            str(Path(self.values["project"]).resolve()),
            "--json",
        ]
        if context:
            command += ["--context", context]
        command += arguments
        completed = subprocess.run(
            command,
            cwd=Path(self.values["project"]).resolve(),
            env=self.direct_env(),
            input=body,
            capture_output=True,
            text=True,
            timeout=60,
        )
        artifact = {
            "command": command,
            "exit_code": completed.returncode,
            "stdout": completed.stdout,
            "stderr": completed.stderr,
        }
        path = self.case / "artifacts" / f"body-{label}.json"
        path.write_text(json.dumps(artifact, indent=2) + "\n")
        if completed.returncode:
            raise RuntimeError(f"{label}: {completed.stderr[-1000:]}")
        return json.loads(completed.stdout)

    @staticmethod
    def transient_attachment(error: RuntimeError):
        text = str(error)
        return any(
            value in text
            for value in (
                "Context belongs to an obsolete connection incarnation",
                "Fresh exact native state is unavailable",
                "SessionRequired",
            )
        )

    def wait_idle(self, actor: str, prefix: str):
        time.sleep(1)
        deadline = time.monotonic() + 240
        while time.monotonic() < deadline:
            self.counter += 1
            try:
                snapshot = self.issue(
                    f"{prefix}-idle-{self.counter:03d}", {"kind": "snapshot"}
                )
            except RuntimeError as error:
                if self.transient_attachment(error):
                    time.sleep(0.2)
                    continue
                raise
            if snapshot["actors"][actor]["native"]["phase"] == "idle":
                return snapshot
            time.sleep(1)
        raise TimeoutError(f"{actor} did not return idle after {prefix}")

    @staticmethod
    def require_participation(snapshot: dict, actor: str, desired: str):
        whoami = snapshot["actors"][actor]["whoami"]["result"]
        if (
            whoami["participation"] != desired
            or whoami["agent"]["joined"] != (desired == "joined")
        ):
            raise AssertionError(f"{actor} did not report participation={desired}")
        return snapshot

    @staticmethod
    def accepted_order(snapshot: dict, actor: str, bodies: list[str]):
        accepted = snapshot["actors"][actor]["acceptances"]
        order = {item["token"]: index for index, item in enumerate(accepted["order"])}
        observed = []
        for body in bodies:
            matches = [
                (token, value["text"].find(body))
                for token, value in accepted["inputs"].items()
                if body in value["text"]
            ]
            if len(matches) != 1:
                return None
            token, offset = matches[0]
            if token not in accepted["receipts"] or token not in order:
                return None
            observed.append((order[token], offset, token))
        if [(index, offset) for index, offset, _ in observed] != sorted(
            (index, offset) for index, offset, _ in observed
        ):
            raise AssertionError("native acceptance order differs from event order")
        return observed

    def wait_accepted(self, actor: str, bodies: list[str], prefix: str):
        time.sleep(2)
        deadline = time.monotonic() + 240
        while time.monotonic() < deadline:
            self.counter += 1
            snapshot = self.issue(
                f"{prefix}-accepted-{self.counter:03d}", {"kind": "snapshot"}
            )
            order = self.accepted_order(snapshot, actor, bodies)
            if order and snapshot["actors"][actor]["native"]["phase"] == "idle":
                return snapshot, order
            time.sleep(1)
        raise TimeoutError(f"{actor} did not accept the expected native bodies")

    def wait_acknowledged(self, minimum: int, prefix: str):
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            self.counter += 1
            snapshot = self.issue(
                f"{prefix}-ack-{self.counter:03d}", {"kind": "snapshot"}
            )
            if (
                snapshot["status"]["result"]["statistics"][
                    "acknowledged_deliveries"
                ]
                >= minimum
            ):
                return snapshot
            time.sleep(0.2)
        raise TimeoutError(f"acknowledged deliveries did not reach {minimum}")

    def wait_delivery_ready(self, actors: list[str], prefix: str):
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline:
            self.counter += 1
            try:
                snapshot = self.issue(
                    f"{prefix}-ready-{self.counter:03d}", {"kind": "snapshot"}
                )
            except RuntimeError as error:
                if self.transient_attachment(error):
                    time.sleep(0.2)
                    continue
                raise
            deliveries = [
                snapshot["actors"][actor]["whoami"]["result"]["native_delivery"]
                for actor in actors
            ]
            if all(
                value["state"] == "ready" and value["head"] is None
                for value in deliveries
            ):
                return snapshot
            time.sleep(0.2)
        raise TimeoutError("participants did not reach a clean native delivery boundary")

    @staticmethod
    def result_items(value):
        return value["result"]["items"]

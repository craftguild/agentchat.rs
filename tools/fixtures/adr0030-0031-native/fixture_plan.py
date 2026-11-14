"""Explicit fresh fixture paths and image inputs for one integrated campaign."""

from dataclasses import dataclass
import os
from pathlib import Path

from actor_producer import Image
from cli_adapter import CallError


@dataclass(frozen=True)
class FixturePlan:
    host: str
    case: Path
    cli: Image
    helper: Image
    native: Image
    native_version: str
    source_seal: Image

    @property
    def project(self):
        return self.case / "project"

    @property
    def state(self):
        return self.case / "state"

    @property
    def config(self):
        return self.case / "config"

    @property
    def profile(self):
        return self.config / "host-config"

    @property
    def artifacts(self):
        return self.case / "artifacts"

    def validate(self):
        if self.host not in ("codex", "claude") or not self.case.is_absolute():
            raise CallError("An explicit host and absolute fresh case are required")
        if self.case.exists() or self.case.is_symlink():
            raise CallError("A native fixture never reuses or removes an existing case")
        if not self.native_version or any(char.isspace() for char in self.native_version):
            raise CallError("The separately observed native version is required")
        for image in (self.cli, self.helper, self.native, self.source_seal):
            image.check()
        if os.environ.get("CLAUDECODE"):
            raise CallError("Nested-session controls must remain intact")

    def environment(self):
        environment = os.environ.copy()
        environment.update(AGENTCHAT_HOME=str(self.state), AGENTCHAT_USER_CONFIG_ROOT=str(self.config))
        environment.pop("AGENTCHAT_CONTEXT", None)
        if self.host == "claude":
            environment["CLAUDE_CONFIG_DIR"] = str(self.profile)
        return environment

    def manifest(self):
        return dict(host=self.host, case=str(self.case), project=str(self.project),
                    state=str(self.state), config=str(self.config), profile=str(self.profile),
                    images={key: dict(path=str(value.path), sha256=value.digest) for key, value in (
                        ("cli", self.cli), ("helper", self.helper), ("native", self.native),
                        ("source_seal", self.source_seal))}, native_version=self.native_version,
                    actor_names=["receipt-receiver", "receipt-sender"],
                    authentication="inherited normal native mechanism; no credential copy",
                    home_inherited=True, codex_home_inherited=True,
                    shared_process_stop=False, native_executed=False)

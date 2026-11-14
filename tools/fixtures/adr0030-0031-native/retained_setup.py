"""Explicit same-path Codex retry after a separately approved normal trust step.

This never sets trust or changes real user configuration. A new attempt owns
new artifacts, images, runtime and native conversations; only the exact prior
fixture's installed project/state/config paths are retained. The ordinary fresh
FixturePlan and its existing-case refusal remain unchanged.
"""

from dataclasses import dataclass, replace
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import uuid

from actor_producer import Image
from cli_adapter import CallError, FixtureCli, save_record
from fixture_plan import FixturePlan
from fixture_setup import FixtureSetup
from native_transport import Codex
from preservation import selected_bytes


RETAINED_FILES = ("artifacts/fixture-inputs.json", "artifacts/project-install.json",
                  "artifacts/project-install.stdout", "artifacts/native-terminal-proof.json",
                  "artifacts/runtime-terminal.json", "project/.codex/config.toml",
                  "artifacts/startup-failed.json", "artifacts/postmortem-event-summary.json")
INSTALL_FILES = {"artifacts/project-install.json", "artifacts/project-install.stdout",
                 "project/.codex/config.toml"}


def preferred_names(case, events):
    """New case/role names never reuse or rename historical native actors."""
    existing = set()
    for event in events:
        if event.get("kind") == "agent.joined":
            handle = event.get("data", {}).get("handle")
            if (not isinstance(handle, str) or len(handle) > 32
                    or re.fullmatch(r"[a-z][a-z0-9]*(?:-[a-z0-9]+)*", handle) is None):
                raise CallError("A historical actor handle is not fully observed")
            existing.add(handle)
    result = {}
    for role in ("receipt-receiver", "receipt-sender"):
        digest = hashlib.sha256(os.fsencode(Path(case).resolve()) + b"\0" + role.encode()).hexdigest()[:12]
        name = role + "-" + digest
        if len(name) > 32 or name in existing or name in result.values():
            raise CallError("The deterministic native name collides; no fallback or retry is allowed")
        result[role] = name
    return result


def mcp_document(cli, state, config, native, version):
    environment = dict(AGENTCHAT_HOME=str(state), AGENTCHAT_USER_CONFIG_ROOT=str(config),
                       AGENTCHAT_CODEX_NATIVE_EXE=str(native), AGENTCHAT_CODEX_NATIVE_VERSION=version)
    return ("[mcp_servers.agentchat]\ncommand=" + json.dumps(str(cli))
            + '\nargs=["mcp"]\n[mcp_servers.agentchat.env]\n'
            + "".join(key + "=" + json.dumps(value) + "\n" for key, value in environment.items())).encode()


def require_absent(original):
    if original.get("uid") != os.geteuid() or type(original.get("pid")) is not int or original["pid"] <= 1:
        raise CallError("No exact original process identity")
    try:
        os.kill(original["pid"], 0)
    except ProcessLookupError:
        return
    except OSError as error:
        raise CallError("Original process absence cannot be observed") from error
    raise CallError("A prior process PID is live; do not reuse this fixture")


def retained_processes(original_case):
    """Conservative path matching can refuse reuse, never authorize a root."""
    result = subprocess.run(["ps", "eww", "-U", str(os.geteuid()), "-o", "pid=,command="],
                            capture_output=True, text=True, timeout=15, check=False)
    if result.returncode:
        raise CallError("Retained fixture process quiescence is unknown")
    for line in result.stdout.splitlines():
        fields = line.strip().split(None, 1)
        if len(fields) == 2 and str(original_case) in fields[1]:
            raise CallError("A process still references the retained fixture; no update allowed")


def native_trust_snapshot(plan, socket_path, user_config):
    if not user_config.path.is_absolute():
        raise CallError("An explicit native user config path is required")
    before, metadata = selected_bytes(user_config.path)
    if hashlib.sha256(before).hexdigest() != user_config.digest:
        raise CallError("Native user config differs from the approved post-trust observation")
    native = Codex(socket_path)
    try:
        result = native.request("config/read", dict(cwd=str(plan.project), includeLayers=True))
    finally:
        native.close()
    layers = result.get("layers", [])
    enabled = [row for row in layers if row.get("name", {}).get("type") == "project"
               and row["name"].get("dotCodexFolder") == str(plan.project / ".codex")]
    users = [row for row in layers if row.get("name", {}).get("type") == "user"
             and row["name"].get("file") == str(user_config.path)]
    trust = result.get("config", {}).get("projects", {}).get(str(plan.project), {}).get("trust_level")
    after, now = selected_bytes(user_config.path)
    if (len(enabled) != 1 or enabled[0].get("disabledReason") is not None or len(users) != 1
            or users[0].get("disabledReason") is not None or trust != "trusted" or before != after
            or (metadata.st_dev, metadata.st_ino, metadata.st_mtime_ns) != (now.st_dev, now.st_ino, now.st_mtime_ns)):
        raise CallError("The native trusted project layer or its user config is uncertain")
    return dict(project=str(plan.project), trust_level=trust, project_layer_enabled=True,
                user_config_path=str(user_config.path), user_config_sha256=user_config.digest)


@dataclass(frozen=True)
class RetainedCodexPlan(FixturePlan):
    original_case: Path
    fixture_root: Path
    original_hashes: dict
    approval_record: Path
    trust_terminal: Image

    @property
    def project(self):
        return self.fixture_root / "project"

    @property
    def state(self):
        return self.fixture_root / "state"

    @property
    def config(self):
        return self.fixture_root / "config"

    def evidence_path(self, name):
        return (self.fixture_root if name in INSTALL_FILES else self.original_case) / name

    def validate(self):
        super().validate()
        if (self.host != "codex" or not self.original_case.is_absolute() or not self.fixture_root.is_absolute()
                or self.case in (self.original_case, self.fixture_root)
                or set(self.original_hashes) != set(RETAINED_FILES)):
            raise CallError("An exact prior Codex fixture and separately fresh attempt are required")
        for path in (self.original_case, self.fixture_root, self.project, self.state, self.config, self.project / ".codex"):
            metadata = path.lstat()
            if (not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != os.geteuid()
                    or path.resolve() != path):
                raise CallError("Retained fixture directories changed ownership or containment")
        approval = json.loads(selected_bytes(self.approval_record)[0])
        if (approval.get("authorization") != "explicit_user_reply"
                or approval.get("project") != str(self.project)
                or approval.get("retain_project_trust") is not True
                or approval.get("fixture_mcp_final_image_update") is not True
                or not approval.get("fixture_update_review_reference")
                or not approval.get("user_reply_reference")):
            raise CallError("Concrete same-project trust and fixture-update authorization is required")
        self.trust_terminal.check()
        trust = json.loads(selected_bytes(self.trust_terminal.path)[0])
        if (trust.get("exit_code") != 0 or trust.get("original_pid_esrch") is not True
                or trust.get("project") != str(self.project)
                or trust.get("native_model_prompt_sent") is not False
                or trust.get("non_target_semantics_unchanged") is not True
                or trust.get("target_entry_retained") != dict(trust_level="trusted")):
            raise CallError("The separate normal trust preflight has no exact terminal proof")
        require_absent(trust["original_birth"])
        self.validate_original()

    def validate_original(self):
        for name, digest in self.original_hashes.items():
            if hashlib.sha256(selected_bytes(self.evidence_path(name))[0]).hexdigest() != digest:
                raise CallError("Retained original evidence or fixture MCP configuration changed")
        read = lambda name: json.loads(selected_bytes(self.evidence_path("artifacts/" + name))[0])
        original = read("fixture-inputs.json")
        if (original.get("host") != "codex" or original.get("case") != str(self.original_case)
                or any(original.get(name) != str(getattr(self, name)) for name in ("project", "state", "config"))
                or (self.original_case / "artifacts/profile-ready.json").exists()):
            raise CallError("Only the exact pre-readiness failed fixture may retain its project")
        install, result = read("project-install.json"), read("project-install.stdout")
        command = install.get("command", [])
        if (install.get("state") != "returned" or install.get("exit_code") != 0
                or install.get("outcome_unknown") is not False or "--request-id" not in command
                or result.get("version") != 1 or "result" not in result or "error" in result
                or result.get("request_id") != command[command.index("--request-id") + 1]):
            raise CallError("The original installation has no exact successful receipt; never replay it")
        project_id = result["project_id"]
        if str(uuid.UUID(project_id)) != project_id:
            raise CallError("The retained project has no canonical namespace")
        page = read("postmortem-event-summary.json")
        events = page.get("events", {})
        head = events.get("head", "0")
        if (page.get("project_id") != project_id or not {"head", "items", "next_cursor", "scan_cursor"}.issubset(events)
                or not isinstance(head, str) or not head.isdecimal()
                or str(int(head)) != head or not 0 < int(head) <= 200 or events.get("next_cursor") is not None
                or events.get("scan_cursor") != head
                or [row.get("seq") for row in events.get("items", [])] != [str(n) for n in range(1, int(head) + 1)]
                or any(row.get("kind") not in {"channel.created", "project.channel_policy_materialized",
                       "agent.joined", "channel.joined", "agent.launch_retired"} for row in events["items"])
                or not isinstance(read("startup-failed.json").get("error"), str)):
            raise CallError("Only complete startup-only history without messages permits this retry")
        for path in (self.original_case / "commands").glob("*.json"):
            command = json.loads(selected_bytes(path)[0])
            if command.get("kind") not in {"inspect", "finish-native", "stop-owned-runtime", "check-selector", "snapshot"}:
                raise CallError("The previous attempt performed more than startup/read-only observation")
        terminal = read("runtime-terminal.json")
        if terminal.get("exit_code") != 0 or terminal.get("scope") != "original owned child only":
            raise CallError("The original dedicated runtime has no normal terminal receipt")
        proof = read("native-terminal-proof.json")
        if set(proof.get("terminal", {})) == {"owner", "supervisor"} and proof.get("original", {}).get("version") == 1:
            owners = [(proof["original"][key], proof["terminal"][key].get("esrch")) for key in ("owner", "supervisor")]
        elif set(proof) == {name + "-" + role for name in ("receipt-receiver", "receipt-sender") for role in ("owner", "supervisor")}:
            owners = [(row["original"], row.get("esrch")) for row in proof.values()]
        else:
            raise CallError("Every original native and supervisor must have terminal proof")
        if len({owner.get("pid") for owner, _esrch in owners}) != len(owners):
            raise CallError("Original native/supervisor proof duplicates a process")
        for original_owner, esrch in owners:
            if esrch is not True or original_owner.get("uid") != os.geteuid():
                raise CallError("Original native termination is uncertain")
            require_absent(original_owner)
        retained_processes(self.original_case)
        if self.original_case != self.fixture_root:
            retained_processes(self.fixture_root)
        images = original["images"]
        expected = mcp_document(images["cli"]["path"], self.state, self.config,
                                images["native"]["path"], original["native_version"])
        if selected_bytes(self.project / ".codex/config.toml")[0] != expected:
            raise CallError("The retained MCP file contains foreign or unexpected configuration")
        return project_id


class RetainedCodexSetup(FixtureSetup):
    """Prepare only after approval and live native trust has been verified."""

    def __init__(self, plan, *, trust_observation, runner=None):
        super().__init__(plan, **({} if runner is None else dict(runner=runner)))
        self.trust_observation = trust_observation

    def prepare(self):
        self.plan.validate()
        # The caller obtains this from existing native config/read. No config
        # write RPC, trust input or guessed trusted state is implemented here.
        observed = self.trust_observation()
        trust = json.loads(selected_bytes(self.plan.trust_terminal.path)[0])
        if (observed.get("project") != str(self.plan.project) or observed.get("trust_level") != "trusted"
                or observed.get("project_layer_enabled") is not True
                or observed.get("user_config_sha256") != trust.get("user_config_after_sha256")):
            raise CallError("The exact project is not normally trusted in the native config layer")
        project_id = self.plan.validate_original()
        history = json.loads(selected_bytes(self.plan.evidence_path("artifacts/postmortem-event-summary.json"))[0])
        self.preferred_names = preferred_names(self.plan.case, history["events"]["items"])
        self.plan.case.mkdir(mode=0o700, parents=True, exist_ok=False)
        for path in (self.plan.artifacts, self.plan.case / "images"):
            path.mkdir(mode=0o700)
        save_record(self.plan.artifacts / "native-preferred-names.json",
                    dict(case=str(self.plan.case), roles=self.preferred_names), exclusive=True)
        frozen = {}
        for name, image in (("cli", self.plan.cli), ("helper", self.plan.helper)):
            image.check()
            target = self.plan.case / "images" / ("agentchat" if name == "cli" else "native_acceptance")
            shutil.copyfile(image.path, target)
            target.chmod(0o700)
            frozen[name] = Image(target, image.digest)
            frozen[name].check()
        self.plan = replace(self.plan, **frozen)
        save_record(self.plan.artifacts / "fixture-inputs.json", dict(
            **self.plan.manifest(), retained_from=str(self.plan.original_case),
            fixture_root=str(self.plan.fixture_root), original_hashes=self.plan.original_hashes,
            trust_observation=observed, preferred_names=self.preferred_names), exclusive=True)
        for name, image, options in (("cli", self.plan.cli, ("--version", "--help")),
                                      ("helper", self.plan.helper, ("--help",))):
            for option in options:
                self._run(name + "-preflight-" + option[2:], [image.path, option], timeout=300)
        path = self.plan.project / ".codex/config.toml"
        original, metadata = selected_bytes(path)
        backup = self.plan.artifacts / "original-project-mcp.toml"
        with backup.open("xb") as output:
            os.chmod(backup, 0o600)
            output.write(original)
        changed = mcp_document(self.plan.cli.path, self.plan.state, self.plan.config,
                               self.plan.native.path, self.plan.native_version)
        intent = dict(operation="fixture_mcp_final_image_update", path=str(path),
                      before_sha256=hashlib.sha256(original).hexdigest(),
                      after_sha256=hashlib.sha256(changed).hexdigest(), outcome_unknown=True)
        save_record(self.plan.artifacts / "mcp-update.json", intent, exclusive=True)
        stage = path.with_name(".matrix-final-image-" + str(uuid.uuid4()))
        with stage.open("xb") as output:
            os.chmod(stage, stat.S_IMODE(metadata.st_mode))
            output.write(changed)
            output.flush()
            os.fsync(output.fileno())
        self.plan.validate_original()
        current, now = selected_bytes(path)
        keys = ("st_dev", "st_ino", "st_uid", "st_mode", "st_mtime_ns", "st_size")
        if current != original or any(getattr(now, key) != getattr(metadata, key) for key in keys):
            raise CallError("Fixture MCP file changed immediately before its approved update")
        stage.replace(path)
        directory = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
        if selected_bytes(path)[0] != changed:
            raise CallError("Fixture MCP publication changed; do not proceed or restore it")
        save_record(self.plan.artifacts / "mcp-update.json", dict(intent, outcome_unknown=False))
        self.client = FixtureCli(image=self.plan.cli.path, image_sha256=self.plan.cli.digest,
                                 project_id=project_id, project=self.plan.project, state=self.plan.state,
                                 config=self.plan.config, artifacts=self.plan.artifacts / "cli")
        return project_id

    def start_producer(self, name):
        recorded = json.loads(selected_bytes(self.plan.artifacts / "native-preferred-names.json")[0])
        if (recorded != dict(case=str(self.plan.case), roles=self.preferred_names)
                or name not in self.preferred_names):
            raise CallError("The recorded role/name plan changed before the native launch")
        return super().start_producer(name, preferred_name=self.preferred_names[name])


def main():
    import argparse
    from run_profile import ProfileCampaign, drive, load_plan
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inputs", type=Path, required=True)
    arguments = parser.parse_args()
    document = json.loads(arguments.inputs.read_text())
    retry = document.pop("retained_codex_retry")
    if (set(retry) != {"schema_version", "original_case", "fixture_root", "original_hashes", "approval_record", "trust_terminal", "user_config"}
            or type(retry["schema_version"]) is not int or retry["schema_version"] != 2):
        raise CallError("Exact retained-project retry inputs are required")
    def image(value):
        if set(value) != {"path", "sha256"}:
            raise CallError("Every retained retry image needs an exact path and SHA-256")
        return Image(Path(value["path"]), value["sha256"])
    base = load_plan(document)
    plan = RetainedCodexPlan(**base.__dict__, original_case=Path(retry["original_case"]),
        fixture_root=Path(retry["fixture_root"]),
        original_hashes=retry["original_hashes"], approval_record=Path(retry["approval_record"]),
        trust_terminal=image(retry["trust_terminal"]))
    campaign = ProfileCampaign(plan, socket_path=document["codex_socket"])
    campaign.setup = RetainedCodexSetup(plan, trust_observation=lambda: native_trust_snapshot(
        plan, document["codex_socket"], image(retry["user_config"])))
    drive(campaign)


if __name__ == "__main__":
    main()

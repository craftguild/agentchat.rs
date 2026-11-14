"""Prepare only a fresh explicit fixture and retain its original owned processes.

No function runs on import. The integration lead supplies the final current
images/source seal before explicit preparation. Host login/trust decisions stay
on the normal host path; this module never copies authentication or changes
native approval/sandbox settings.
"""

from dataclasses import replace
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import stat
import subprocess
import time
import uuid

from actor_producer import ActorProducer, Image
from cli_adapter import CallError, FixtureCli, save_record
from owner_facts import facts, matches
from frame_audit import identifier


def runtime_endpoint(value, *, state, project_id, birth, observe=matches, metadata=None):
    """Locate the product's short endpoint; its path is never native authority.

    RuntimePaths hashes the selected data namespace into /tmp to remain below
    Unix socket path limits. Preserve exact namespace/type/UID/permissions and
    original runtime birth checks instead of assuming a socket below state.
    """
    identifier(project_id)
    identifier(value["instance"])
    data = (Path(state) / project_id).resolve()
    digest = hashlib.sha256(os.fsencode(data)).hexdigest()[:32]
    directory = Path("/tmp") / f"agentchat-{os.geteuid()}-{digest}"
    expected = directory / "rpc.sock"
    if (value.get("candidate_ready") is not True or value.get("admitted") is not False
            or Path(value["socket"]).resolve() != expected.resolve() or not observe(birth)):
        raise CallError("Candidate readiness does not belong to the original fixture runtime")
    metadata = metadata or (lambda path: path.lstat())
    identities = {}
    for label, path, kind, mode in (("directory", directory, stat.S_ISDIR, 0o700),
                                    ("socket", expected, stat.S_ISSOCK, 0o600),
                                    ("lock", data / "runtime.lock", stat.S_ISREG, 0o600)):
        info = metadata(path)
        if not kind(info.st_mode) or info.st_uid != os.geteuid() or stat.S_IMODE(info.st_mode) != mode:
            raise CallError("Runtime endpoint ownership/type/permissions changed")
        identities[label] = dict(path=str(path), device=info.st_dev, inode=info.st_ino)
    return dict(socket=str(expected), instance_id=value["instance"], filesystem=identities)


class OwnedProcess:
    def __init__(self, *, label, command, project, environment, artifacts):
        self.label = label
        self.artifacts = Path(artifacts)
        save_record(self.artifacts / (label + "-intent.json"),
                    dict(command=command, state="intent", outcome_unknown=True), exclusive=True)
        self.output = (self.artifacts / (label + ".log")).open("xb")
        os.chmod(self.output.name, 0o600)
        self.child = subprocess.Popen(command, cwd=project, env=environment, stdin=subprocess.DEVNULL,
                                      stdout=self.output, stderr=subprocess.STDOUT)
        self.birth = facts(self.child.pid)
        save_record(self.artifacts / (label + "-owner.json"),
                    dict(command=command, birth=self.birth, scope="only this explicitly spawned child"),
                    exclusive=True)

    def stop(self, *, interrupt=False):
        if self.child.poll() is None:
            if not matches(self.birth):
                raise CallError("Owned child continuity is uncertain; no signal sent")
            self.child.send_signal(signal.SIGINT if interrupt else signal.SIGTERM)
            # A timeout is retained for human inspection. No shared server or
            # broad PID/name-based kill fallback exists.
            self.child.wait(timeout=30)
        self.output.close()
        save_record(self.artifacts / (self.label + "-terminal.json"),
                    dict(exit_code=self.child.returncode, scope="original owned child only"), exclusive=True)


class FixtureSetup:
    def __init__(self, plan, *, runner=subprocess.run):
        self.plan = plan
        self.runner = runner
        self.runtime = self.viewer = None
        self.runtime_ready = False
        self.client = None
        self.plugin = None
        self.producers = []

    def _run(self, label, command, *, timeout=120):
        started = time.monotonic()
        record = dict(command=[str(value) for value in command], state="intent", outcome_unknown=True,
                      started_at_unix=time.time(), timeout_seconds=timeout)
        destination = self.plan.artifacts / (label + ".json")
        save_record(destination, record, exclusive=True)
        try:
            result = self.runner(record["command"], cwd=self.plan.project, env=self.plan.environment(),
                                 capture_output=True, timeout=timeout, check=False)
        except subprocess.TimeoutExpired as error:
            (self.plan.artifacts / (label + ".stdout")).write_bytes(error.stdout or b"")
            (self.plan.artifacts / (label + ".stderr")).write_bytes(error.stderr or b"")
            record.update(state="timed_out", outcome_unknown=True, elapsed_seconds=time.monotonic() - started)
            save_record(destination, record)
            raise CallError("Fixture operation timed out; inspect the original intent without replay") from error
        except OSError as error:
            record.update(state="spawn_error", outcome_unknown=False, error=type(error).__name__)
            save_record(destination, record)
            raise CallError("Fixture operation could not start") from error
        (self.plan.artifacts / (label + ".stdout")).write_bytes(result.stdout)
        (self.plan.artifacts / (label + ".stderr")).write_bytes(result.stderr)
        record.update(state="returned", exit_code=result.returncode, outcome_unknown=False,
                      elapsed_seconds=time.monotonic() - started)
        save_record(destination, record)
        if result.returncode:
            raise CallError("Explicit fixture preparation failed; retain the original operation")
        return result.stdout

    def prepare(self):
        self.plan.validate()
        self.plan.case.mkdir(parents=True, mode=0o700, exist_ok=False)
        for path in (self.plan.project, self.plan.state, self.plan.config,
                     self.plan.artifacts, self.plan.case / "images"):
            path.mkdir(mode=0o700)
        frozen = {}
        for name, image in (("cli", self.plan.cli), ("helper", self.plan.helper)):
            image.check()
            destination = self.plan.case / "images" / ("agentchat" if name == "cli" else "native_acceptance")
            shutil.copyfile(image.path, destination)
            destination.chmod(0o700)
            frozen[name] = Image(destination, image.digest)
            frozen[name].check()
        self.plan = replace(self.plan, **frozen)
        save_record(self.plan.artifacts / "fixture-inputs.json", self.plan.manifest(), exclusive=True)
        # Ordinary read-only startup checks precede installation. Their cold
        # startup time is retained separately and is no performance certificate.
        for name, image, options in (("cli", self.plan.cli, ("--version", "--help")),
                                      ("helper", self.plan.helper, ("--help",))):
            for option in options:
                self._run(name + "-preflight-" + option[2:], [image.path, option], timeout=300)
        self._run("git-init", ["git", "init", "-q", str(self.plan.project)])
        request_id = str(uuid.uuid4())
        installed = json.loads(self._run("project-install", [
            self.plan.cli.path, "--project", self.plan.project, "--request-id", request_id,
            "--json", "install", "--scope", "directory", "--new-project", "--default-channels", "native"]))
        if (type(installed.get("version")) is not int or installed["version"] != 1
                or installed.get("request_id") != request_id
                or "result" not in installed or "error" in installed):
            raise CallError("Fresh project installation returned no current success envelope")
        project_id = installed["project_id"]
        if str(uuid.UUID(project_id)) != project_id:
            raise CallError("Fresh project returned no exact namespace")
        if self.plan.host == "claude":
            self._run("native-install", [self.plan.helper.path, "install", self.plan.case,
                      self.plan.config, self.plan.native.path, "--agentchat", self.plan.cli.path,
                      "--version", self.plan.native_version])
            launch = json.loads(self._run("native-launch-plan", [self.plan.helper.path, "launch-plan",
                                self.plan.state, self.plan.config, self.plan.cli.path]))
            if (Path(launch["settings"]) != self.plan.profile / "settings.json"
                    or Path(launch["registry"]) != self.plan.profile / ".claude.json"):
                raise CallError("The normal owned install escaped the fresh contained profile")
            self.plugin = Path(launch["plugin"])
        else:
            # This is the native daemon's ordinary project configuration layer.
            # The explicit acceptance supervisor still verifies its live daemon,
            # exact physical owner and root. No trust/approval value is supplied.
            directory = self.plan.project / ".codex"
            directory.mkdir(mode=0o700)
            environment = dict(AGENTCHAT_HOME=str(self.plan.state),
                               AGENTCHAT_USER_CONFIG_ROOT=str(self.plan.config),
                               AGENTCHAT_CODEX_NATIVE_EXE=str(self.plan.native.path),
                               AGENTCHAT_CODEX_NATIVE_VERSION=self.plan.native_version)
            document = ("[mcp_servers.agentchat]\ncommand=" + json.dumps(str(self.plan.cli.path))
                        + '\nargs=["mcp"]\n[mcp_servers.agentchat.env]\n'
                        + "".join(key + "=" + json.dumps(value) + "\n" for key, value in environment.items()))
            with (directory / "config.toml").open("x") as output:
                output.write(document)
        self.client = FixtureCli(image=self.plan.cli.path, image_sha256=self.plan.cli.digest,
                                 project_id=project_id, project=self.plan.project, state=self.plan.state,
                                 config=self.plan.config, artifacts=self.plan.artifacts / "cli",
                                 profile=self.plan.profile if self.plan.host == "claude" else None)
        return project_id

    def start_runtime(self, *, hold_accepted=None, label="runtime"):
        if not self.client or self.runtime and self.runtime.child.poll() is None:
            raise CallError("Prepared fixture and no surviving owned runtime are required")
        self.plan.helper.check()
        command = [str(self.plan.helper.path), "codex-serve" if self.plan.host == "codex" else "serve",
                   str(self.plan.state), str(self.plan.project), "--version", self.plan.native_version]
        if hold_accepted is not None:
            marker = Path(hold_accepted)
            if marker.parent.resolve() != self.plan.state.resolve() or marker.exists():
                raise CallError("Acceptance cut requires an absent marker directly in this fixture state")
            command += ["--hold-accepted", str(marker)]
        self.runtime = OwnedProcess(label=label, command=command, project=self.plan.project,
                                    environment=self.plan.environment(), artifacts=self.plan.artifacts)
        self.runtime_ready = False

    def poll_runtime_ready(self):
        if not self.runtime or self.runtime.child.poll() is not None:
            raise CallError("Original owned runtime exited before readiness")
        values = []
        for line in (self.plan.artifacts / (self.runtime.label + ".log")).read_bytes().splitlines():
            try:
                value = json.loads(line)
            except ValueError:
                continue
            if isinstance(value, dict) and value.get("candidate_ready") is True:
                values.append(value)
        if not values:
            return False
        if len(values) != 1:
            raise CallError("Candidate readiness does not belong to the original fixture runtime")
        endpoint = runtime_endpoint(values[0], state=self.plan.state, project_id=self.client.project_id,
                                    birth=self.runtime.birth)
        if not self.runtime_ready:
            # Actor-free Doctor checks the actual typed handshake's namespace
            # and instance. It cannot initialize/join or manufacture an actor.
            doctor = self.client._call(self.runtime.label + "-ready-doctor", ["doctor"])["result"]
            health = doctor["runtime"]
            if health.get("reachable") is not True or health.get("instance_id") != endpoint["instance_id"]:
                raise CallError("Actual runtime handshake differs from the original candidate instance")
            if not matches(self.runtime.birth):
                raise CallError("Original runtime birth changed during readiness inspection")
            save_record(self.plan.artifacts / (self.runtime.label + "-ready.json"),
                        dict(ready=values[0], endpoint=endpoint, doctor=doctor, owner=self.runtime.birth), exclusive=True)
        self.runtime_ready = True
        return True

    def start_setup_observer(self, *, label="setup-viewer"):
        if (not self.runtime_ready or not self.runtime or self.runtime.child.poll() is not None
                or self.viewer is not None and self.viewer.child.poll() is None):
            raise CallError("Original owned runtime is required before the setup observer")
        self.plan.cli.check()
        # The product Viewer is the dedicated context-free observer lease. Its
        # startup writes only the loopback URL; no browser or generic watch runs.
        command = [str(self.plan.cli.path), "--project", str(self.plan.project), "--json", "viewer"]
        self.viewer = OwnedProcess(label=label, command=command, project=self.plan.project,
                                   environment=self.plan.environment(), artifacts=self.plan.artifacts)

    def start_producer(self, name, *, preferred_name=None):
        if not self.runtime_ready or not self.client or not self.runtime or self.runtime.child.poll() is not None:
            raise CallError("Prepared fixture and original owned runtime are required")
        if len(self.producers) >= 2 or any(value.name == name for value in self.producers):
            raise CallError("The accepted matrix has exactly two fresh owned producers")
        producer = ActorProducer(host=self.plan.host, name=name, project_id=self.client.project_id,
                                 project=self.plan.project, state=self.plan.state, config=self.plan.config,
                                 cli=self.plan.cli, helper=self.plan.helper, native=self.plan.native,
                                 version=self.plan.native_version,
                                 artifacts=self.plan.artifacts / name,
                                 preferred_name=preferred_name,
                                 profile=self.plan.profile if self.plan.host == "claude" else None,
                                 plugin=self.plugin)
        producer.start()
        self.producers.append(producer)
        return producer

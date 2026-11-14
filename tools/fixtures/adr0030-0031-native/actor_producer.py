"""Owned PTY producers connected to shared managed-startup readiness evidence.

Creating this object performs no native operation. Explicit start requires exact
frozen image digests and a prepared disposable fixture. Normal host prompts are
retained verbatim; terminal protocol replies never answer trust or permission.
"""

from dataclasses import dataclass
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import re
import select
import struct
import subprocess
import termios
import threading
import time
import uuid

from cli_adapter import CallError, NativeActor, save_record
from frame_audit import identifier
from resource_probe import file_fact
from owner_facts import facts, matches


def sha256(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def configuration_snapshot():
    """Selected config hashes only; native authentication files are excluded."""
    home = Path.home()
    paths = (home / ".codex/config.toml", home / ".codex/hooks.json",
             home / ".claude/settings.json", home / ".claude.json")
    return dict(observed_unix_ns=time.time_ns(), files=[file_fact(path) for path in paths],
                qualification="hash/metadata observation only; no contents retained or preservation success inferred")


@dataclass(frozen=True)
class Image:
    path: Path
    digest: str

    def check(self):
        if not self.path.is_absolute() or sha256(self.path) != self.digest:
            raise CallError("A required current image is absent or changed")


def correlate_ready(value, *, host, project_id, project, supervisor_pid, observe=matches):
    """Correlate a route with the physical producer; never infer its root."""
    binding = value["binding"]
    context = binding["context"]
    proof = json.loads(binding["identity"]["owner"])
    if (type(value.get("version")) is not int or value["version"] != 1 or context["host"] != host
            or context["project_id"] != project_id
            or Path(context["working_directory"]).resolve() != Path(project).resolve()
            or proof["supervisor"]["pid"] != supervisor_pid
            or not context["host_session_id"] or not context["context_id"]):
        raise CallError("Native readiness does not belong to this exact owned producer")
    for token in (proof["supervisor"], proof["owner"]):
        if not observe(token):
            raise CallError("Original physical readiness is no longer live")
    return NativeActor(host, context["host_session_id"]), context, proof


def readiness_aliases(rows, **expected):
    """Collapse only the same complete binding, not distinct candidate roots."""
    selected = {}
    for path, value in rows:
        actor, context, proof = correlate_ready(value, **expected)
        generation = value["binding"]["identity"]["generation"]
        identifier(generation)
        identity = dict(context=context, proof=proof, generation=generation,
                        version=value["version"], control=value["control"], name=value["preferred_name"])
        key = json.dumps(identity, sort_keys=True, separators=(",", ":"))
        selected.setdefault(key, []).append((Path(path), actor, context, proof, generation))
    if not selected:
        return None
    if len(selected) != 1:
        raise CallError("Distinct native readiness bindings are ambiguous")
    aliases = next(iter(selected.values()))
    original = [row for row in aliases if row[0].name == row[4] + ".json"]
    if not original:
        # The aliases are published separately. Only the canonical original
        # receipt authorizes progress; observing indexes first is still pending.
        return None
    if len(original) != 1:
        raise CallError("The exact original generation receipt is ambiguous")
    return original[0]


def published_context(result, reports, *, actor, original, proof, generation):
    """A public selected snapshot chooses the current report, never its mtime."""
    if result.get("participation") != "joined" or result.get("actor") is None:
        return None
    subject = result["actor"]
    if (result["project_id"] != original["project_id"] or subject.get("kind") != "agent"
            or subject.get("host") != actor.host or subject.get("chat_id") != actor.chat_id
            or result.get("native_delivery", {}).get("generation") != generation):
        raise CallError("Public selector changed the exact native actor or original generation")
    identifier(result["context_id"])
    candidates = [original] if result["context_id"] == original["context_id"] else []
    for report in reports:
        context = report.get("context")
        if not context or context.get("context_id") != result["context_id"]:
            continue
        launch = report["launch"]
        if (launch["generation"] != generation or json.loads(launch["owner"]) != proof
                or any(context[key] != original[key] for key in (
                    "host", "host_session_id", "project_id", "working_directory", "host_version", "os_profile"))
                or report.get("agent_id", subject["actor_id"]) != subject["actor_id"]):
            raise CallError("Current report differs from original physical owner/root/generation")
        candidates.append(context)
    unique = {json.dumps(value, sort_keys=True): value for value in candidates}
    if not unique:
        return None
    if len(unique) != 1:
        raise CallError("Current descriptor reports disagree")
    context = next(iter(unique.values()))
    if context["incarnation"] != subject["incarnation"]:
        raise CallError("Public actor incarnation differs from the published descriptor")
    return context


class Terminal:
    def __init__(self, command, *, project, environment, artifacts, configuration_baseline=None):
        self.artifacts = Path(artifacts)
        self.artifacts.mkdir(mode=0o700, parents=True, exist_ok=False)
        self.raw = self.artifacts / "terminal.raw"
        self.raw.touch(mode=0o600)
        if configuration_baseline is not None:
            save_record(self.artifacts / "pre-native-config-hashes.json", configuration_baseline, exclusive=True)
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 44, 160, 0, 0))
        def controlling_terminal():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)
        try:
            self.child = subprocess.Popen(command, cwd=project, env=environment,
                                          stdin=slave, stdout=slave, stderr=slave,
                                          preexec_fn=controlling_terminal)
        finally:
            os.close(slave)
        self.birth = facts(self.child.pid)
        self.closed = threading.Event()
        self.lock = threading.Lock()
        self.failure = None
        self.thread = threading.Thread(target=self._drain, daemon=True)
        self.thread.start()
        save_record(self.artifacts / "spawn.json", dict(command=command, supervisor=self.birth), exclusive=True)

    def _drain(self):
        try:
            while not self.closed.is_set():
                if not select.select([self.master], [], [], .05)[0]:
                    continue
                try:
                    block = os.read(self.master, 65_536)
                except OSError:
                    return
                if not block:
                    return
                with self.lock:
                    with self.raw.open("ab") as output:
                        output.write(block)
                    for question, answer in ((b"\x1b[6n", b"\x1b[1;1R"),
                                             (b"\x1b[c", b"\x1b[?1;2c")):
                        if question in block:
                            os.write(self.master, answer)
        except BaseException as error:
            self.failure = type(error).__name__

    def submit(self, label, text):
        if not matches(self.birth) or self.child.poll() is not None or self.failure:
            raise CallError("Owned native producer is unavailable")
        intent = self.artifacts / (label + ".json")
        save_record(intent, dict(state="intent", text=text, sha256=hashlib.sha256(text.encode()).hexdigest(),
                                 outcome_unknown=True), exclusive=True)
        with self.lock:
            # Bracketed paste keeps literal newlines inside this single prompt.
            payload = b"\x1b[200~" + text.encode() + b"\x1b[201~"
            view = memoryview(payload)
            while view:
                view = view[os.write(self.master, view):]
        time.sleep(.2)
        os.write(self.master, b"\r")
        # Submission is not model completion, tool success or native acceptance.
        save_record(intent, dict(state="submitted", sha256=hashlib.sha256(text.encode()).hexdigest(),
                                 outcome_unknown=True))

    def decision(self, *, label, terminal_sha256, key, authorization):
        keys = {"enter": b"\r", "escape": b"\x1b", "cancel": b"\x03",
                "up": b"\x1b[A", "down": b"\x1b[B"}
        if authorization != "explicit_user_reply" or key not in keys:
            raise CallError("A concrete normal-host decision is required")
        with self.lock:
            if sha256(self.raw) != terminal_sha256 or not matches(self.birth):
                raise CallError("Native decision observation changed")
            save_record(self.artifacts / (label + ".json"),
                        dict(authorization=authorization, terminal_sha256=terminal_sha256, key=key),
                        exclusive=True)
            os.write(self.master, keys[key])

    def finish_native(self, label, host):
        if host not in ("codex", "claude") or not matches(self.birth) or self.child.poll() is not None:
            raise CallError("No exact live owned terminal for normal native exit")
        text = "/quit" if host == "codex" else "/exit"
        save_record(self.artifacts / (label + ".json"),
                    dict(operation="normal_owned_fixture_exit", text=text, supervisor=self.birth,
                         native_approval_decision=False, outcome_unknown=True), exclusive=True)
        with self.lock:
            os.write(self.master, text.encode())
        time.sleep(.2)
        os.write(self.master, b"\r")

    def cosmetic_theme(self, *, label, terminal_sha256):
        """Confirm only the displayed initial Claude theme's cosmetic default."""
        with self.lock:
            raw = self.raw.read_bytes()
            plain = re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", raw)
            compact = b"".join(plain.split())
            if (hashlib.sha256(raw).hexdigest() != terminal_sha256 or not matches(self.birth)
                    or self.child.poll() is not None
                    or b"Choosethetextstylethatlooksbestwithyourterminal" not in compact
                    or b"Darkmode" not in compact or b"/theme" not in compact
                    or any(word in compact.lower() for word in (b"login", b"trust", b"permission", b"oauth"))):
                raise CallError("No unchanged initial cosmetic theme observation")
            save_record(self.artifacts / (label + ".json"),
                        dict(operation="initial_cosmetic_theme_default", terminal_sha256=terminal_sha256,
                             supervisor=self.birth, authority="fixture_cosmetic_default",
                             native_approval_decision=False, outcome_unknown=True), exclusive=True)
            os.write(self.master, b"\r")

    def close_observer(self):
        # Closing this local reader is separate from stopping native work.
        self.closed.set()
        self.thread.join(timeout=3)
        os.close(self.master)


class ActorProducer:
    def __init__(self, *, host, name, project_id, project, state, config,
                 cli, helper, native, version, artifacts, profile=None, plugin=None, preferred_name=None):
        if host not in ("codex", "claude") or not name or not all(
                char.isascii() and (char.isalnum() or char == "-") for char in name):
            raise CallError("An exact host and safe fixture actor label are required")
        self.host, self.name, self.project_id = host, name, project_id
        if preferred_name is not None and (not isinstance(preferred_name, str) or len(preferred_name) > 32
                or re.fullmatch(r"[a-z][a-z0-9]*(?:-[a-z0-9]+)*", preferred_name) is None):
            raise CallError("The explicit native preferred name must be a bounded lowercase slug")
        # The role remains stable for artifacts and controller keys. A retained
        # retry may select a distinct preferred handle for its new native root.
        self.preferred_name = preferred_name
        self.project, self.state, self.config = map(Path, (project, state, config))
        self.cli, self.helper, self.native = cli, helper, native
        self.version, self.artifacts = version, Path(artifacts)
        self.profile, self.plugin = profile, plugin
        self.terminal = None
        self.actor = self.context = self.proof = None
        self.original_context = self.launch_generation = None
        self.environment = os.environ.copy()
        self.environment.update(AGENTCHAT_HOME=str(state), AGENTCHAT_USER_CONFIG_ROOT=str(config),
                                TERM="xterm-256color")
        self.environment.pop("AGENTCHAT_CONTEXT", None)
        if profile is not None:
            self.environment["CLAUDE_CONFIG_DIR"] = str(profile)

    def start(self):
        if self.terminal is not None:
            raise CallError("A producer cannot silently launch a replacement root")
        if os.environ.get("CLAUDECODE"):
            raise CallError("The native nested-session guard must remain intact")
        for image in (self.cli, self.helper, self.native):
            image.check()
        report = self.artifacts / "authorized.json"
        if self.host == "codex":
            command = [str(self.helper.path), "codex-startup", str(self.state), str(self.project),
                       str(self.native.path), str(self.config), str(report), "--name", self.preferred_name or self.name]
        else:
            if not self.profile or not self.plugin:
                raise CallError("Prepared owned Claude settings/plugin paths are required")
            command = [str(self.helper.path), "startup", str(self.state), str(self.project),
                       str(self.native.path), str(self.config), str(self.profile), str(self.plugin),
                       str(Path(self.profile) / "settings.json"), str(report),
                       "--root", str(uuid.uuid4()), "--version", self.version,
                       "--agentchat", str(self.cli.path), "--start-launch", "--configured",
                       "--name", self.preferred_name or self.name]
        self.terminal = Terminal(command, project=self.project, environment=self.environment,
                                 artifacts=self.artifacts, configuration_baseline=configuration_snapshot())

    def terminal_status(self):
        code = self.terminal.child.poll()
        path = self.artifacts / "post-native-config-hashes.json"
        if code is not None and not path.exists():
            save_record(path, configuration_snapshot(), exclusive=True)
        return code

    def readiness(self):
        if self.terminal is None or self.terminal.child.poll() is not None:
            raise CallError("Owned producer has not started or has exited")
        selected = []
        for path in (self.state / "hosts/launches/ready").glob("*.json"):
            try:
                value = json.loads(path.read_text())
                raw = json.loads(value["binding"]["identity"]["owner"])
                if raw["supervisor"]["pid"] != self.terminal.child.pid:
                    continue
                selected.append((path, value))
            except FileNotFoundError:
                continue
        chosen = readiness_aliases(selected, host=self.host, project_id=self.project_id,
                                   project=self.project, supervisor_pid=self.terminal.child.pid)
        if chosen is None:
            return False
        ready_path, actor, context, proof, generation = chosen
        if self.actor and (actor != self.actor or proof != self.proof
                           or generation != self.launch_generation or context != self.original_context):
            raise CallError("A native root or original physical proof changed")
        if self.actor is None:
            self.actor, self.context, self.proof = actor, context, proof
            self.original_context, self.launch_generation = context, generation
        self.ready_path = ready_path
        return True

    def activation_reported(self):
        """Physical readiness alone does not imply a committed actor association."""
        if not self.readiness():
            return False
        try:
            report = json.loads((self.artifacts / "authorized.activated.json").read_text())
        except FileNotFoundError:
            return False
        context, launch = report["context"], report["launch"]
        if (launch["generation"] != self.launch_generation or json.loads(launch["owner"]) != self.proof
                or any(context[key] != self.original_context[key] for key in (
                    "host", "host_session_id", "project_id", "working_directory", "host_version", "os_profile"))
                or report.get("shared_supervisor") is not True
                or not (report.get("membership_committed") is True or report.get("membership_established") is True)):
            raise CallError("Initial activation report differs from the original physical readiness")
        for value in (report["agent_id"], context["context_id"], context["incarnation"]):
            identifier(value)
        # This report only gates the first read; the public selector still
        # chooses the current descriptor and verifies actual participation.
        return True

    def selected_whoami(self, client, label):
        if not self.readiness():
            raise CallError("Exact native readiness is still pending")
        for attempt in range(4):
            result = client.whoami(label + f"-route-{attempt}", self.actor)["result"]
            reports = []
            for name in ("authorized.json", "authorized.activated.json", "authorized.reattached.json"):
                try:
                    reports.append(json.loads((self.artifacts / name).read_text()))
                except FileNotFoundError:
                    continue
            context = published_context(result, reports, actor=self.actor, original=self.original_context,
                                        proof=self.proof, generation=self.launch_generation)
            if context is not None:
                if self.preferred_name is not None and result["actor"]["handle"] != self.preferred_name:
                    raise CallError("The public selected handle differs from the recorded preferred name")
                if not self.readiness():
                    raise CallError("Original physical readiness changed during selected read")
                self.context = context
                return result
            time.sleep(.05)
        raise CallError("Current public descriptor/report did not converge within the bounded startup read")

    def _idle(self, idle_observation):
        if not self.readiness() or idle_observation.get("root") != self.actor.chat_id:
            raise CallError("A fresh exact-root native idle observation is required")
        if idle_observation.get("phase") != "idle" or idle_observation.get("pending_requests"):
            raise CallError("Native work or a normal permission decision is still pending")
        observed = idle_observation.get("observed_monotonic_ns")
        if type(observed) is not int or observed <= 0:
            raise CallError("Native idle observation has no positive monotonic timestamp")
        age = time.monotonic_ns() - observed
        if (not 0 <= age <= 10_000_000_000
                or idle_observation.get("context_id") != self.context["context_id"]
                or idle_observation.get("owner") != self.proof["owner"]):
            raise CallError("Native idle observation is stale or belongs to another owner/descriptor")

    def prompt(self, label, text, *, idle_observation):
        self._idle(idle_observation)
        self.terminal.submit(label, text)

    def finish(self, label, *, idle_observation):
        self._idle(idle_observation)
        self.terminal.finish_native(label, self.host)

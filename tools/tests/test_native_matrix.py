"""A proof decoder must reject malformed frames rather than certify fixtures."""
import importlib.util
import copy
import hashlib
import json
from pathlib import Path
import sys
import os
import subprocess
import stat
import tempfile
import unittest
from unittest import mock
import uuid


path = Path(__file__).resolve().parents[1] / "fixtures/adr0030-0031-native/frame_audit.py"
spec = importlib.util.spec_from_file_location("frame_audit", path)
frames = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = frames
spec.loader.exec_module(frames)
receipt_spec = importlib.util.spec_from_file_location("native_matrix_receipts", path.with_name("receipt_audit.py"))
receipts = importlib.util.module_from_spec(receipt_spec)
receipt_spec.loader.exec_module(receipts)
cli_spec = importlib.util.spec_from_file_location("native_matrix_cli", path.with_name("cli_adapter.py"))
cli = importlib.util.module_from_spec(cli_spec)
sys.modules[cli_spec.name] = cli
cli_spec.loader.exec_module(cli)
sys.modules["cli_adapter"] = cli
sys.path.insert(0, str(path.parent))
import actor_producer
import fixture_plan
import fixture_setup
import native_observer
import phase_controller
import phase_commands
import preservation
import resource_probe
import run_profile
import recovery_controller
import retained_setup


def message(body, ack=True, read=True):
    identifier = str(uuid.UUID(int=1))
    metadata = dict(project_id=identifier, generation=identifier, delivery_id=identifier,
                    auto_ack=ack, auto_read=read, event_id=identifier, seq="9007199254740993",
                    message_id=identifier, thread_id=identifier,
                    author=dict(actor_id=identifier, kind="user", handle="user"),
                    channel_id=identifier, channel="native", addressed=True,
                    body_utf8_bytes=len(body.encode()))
    header = json.dumps(metadata, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    return f"[AgentChat:Message]\n--\n{header}\n--\n{body}\n--\n[/AgentChat:Message]"


def event(ack, read):
    identifier = str(uuid.UUID(int=1))
    metadata = dict(project_id=identifier, generation=identifier, delivery_id=identifier,
                    auto_ack=ack, auto_read=read,
                    event=dict(version=1, event_id=identifier, seq="9",
                               kind="message.delivery_confirmation_timeout",
                               occurred_at="2026-10-28T00:00:00.000Z", actor_id=None,
                               channel_id=identifier, message_id=identifier, data={}),
                    addressed=True, data_omitted=False)
    header = json.dumps(metadata, ensure_ascii=False, separators=(",", ":"))
    return f"[AgentChat:Event]\n--\n{header}\n--\n[/AgentChat:Event]"


class NativeFrameProofTests(unittest.TestCase):
    def test_complete_unicode_controls_and_marker_lookalikes_keep_exact_bytes(self):
        tail = "日本語😀\r\n--\n[/AgentChat:Message]\t \n"
        body = "\0" * (65_536 - len(tail.encode())) + tail
        for ack in (False, True):
            for read in (False, True):
                actual = frames.decode_frame(message(body, ack, read))
                self.assertEqual(actual.body.encode(), body.encode())
                actual.require_policy(ack, read)
                self.assertEqual(actual.metadata["seq"], "9007199254740993")

    def test_missing_wrong_duplicate_policy_and_body_count_are_refused(self):
        text = message("日本語😀\r\n")
        for changed in (
            text.replace('"auto_ack":true,', ""),
            text.replace('"auto_read":true,', ""),
            text.replace('"auto_ack":true', '"auto_ack":1'),
            text.replace('"auto_read":true', '"auto_read":null'),
            text.replace('"auto_ack":true', '"auto_ack":true,"auto_ack":true'),
            text.replace('"body_utf8_bytes":15', '"body_utf8_bytes":1'),
            text + "\n",
        ):
            with self.assertRaises(frames.AuditError):
                frames.decode_frame(changed)


class CanonicalReceiptProofTests(unittest.TestCase):
    def row(self):
        identifier = str(uuid.UUID(int=1))
        return dict(recipient=dict(actor_id=identifier, kind="agent", handle="receiver"),
                    delivery=dict(delivery_id=identifier, state="inflight", accepted_at=None,
                                  acknowledged_at=None,
                                  confirmation=dict(deadline="2026-10-28T00:01:00.000Z", state="waiting")),
                    reads=[])

    def test_early_ack_does_not_certify_acceptance_or_suppress_terminal_timeout(self):
        row = self.row()
        row["delivery"].update(acknowledged_at="2026-10-28T00:00:01.000Z", state="acknowledged")
        receipts.require_facts(row, accepted=False, acknowledged=True, kinds=[], confirmation="waiting")
        row["delivery"]["confirmation"]["state"] = "timed_out"
        row["delivery"]["accepted_at"] = "2026-10-28T00:01:01.000Z"
        receipts.require_facts(row, accepted=True, acknowledged=True, kinds=[], confirmation="timed_out")
        with self.assertRaises(frames.AuditError):
            receipts.require_facts(row, accepted=True, acknowledged=True, kinds=[], confirmation="confirmed")

    def test_read_kinds_and_user_history_never_fabricate_native_deliveries(self):
        row = self.row()
        row["recipient"]["kind"] = "user"
        row["delivery"] = None
        row["reads"] = [dict(kind="explicit", at="2026-10-28T00:00:01.000Z")]
        receipts.canonical_row(row)
        wrong = copy.deepcopy(row)
        wrong["reads"][0]["kind"] = "automatic"
        with self.assertRaises(frames.AuditError):
            receipts.canonical_row(wrong)
        wrong = self.row()
        wrong["reads"] = [dict(kind="automatic", at="2026-10-28T00:00:01.000Z")]
        with self.assertRaises(frames.AuditError):
            receipts.canonical_row(wrong)

    def test_detail_pages_reject_duplicate_projection_and_actors(self):
        identifier = str(uuid.UUID(int=1))
        detail = dict(message=dict(id=identifier, body="body"), receipts=dict(
            items=[self.row()], next_cursor=None, scan_cursor="1", head="1"))
        receipts.detail_page(detail, identifier)
        wrong = copy.deepcopy(detail)
        wrong["readers"] = []
        with self.assertRaises(frames.AuditError):
            receipts.detail_page(wrong, identifier)
        wrong = copy.deepcopy(detail)
        wrong["receipts"]["items"] *= 2
        with self.assertRaises(frames.AuditError):
            receipts.detail_page(wrong, identifier)

    def test_historical_positive_events_do_not_hide_new_receipt_notifications(self):
        receipts.no_positive_receipt_events([dict(seq="1", kind="message.read")], after="2")
        with self.assertRaises(frames.AuditError):
            receipts.no_positive_receipt_events([dict(seq="3", kind="message.read")], after="2")

    def test_events_are_bounded_typed_and_never_substitute_for_messages(self):
        for ack in (False, True):
            for read in (False, True):
                actual = frames.decode_frame(event(ack, read))
                actual.require_policy(ack, read)
                self.assertEqual(actual.kind, "event")
                self.assertIsNone(actual.body)
        text = event(True, True)
        for changed in (
            text.replace('"auto_read":true,', ""),
            text.replace('"data_omitted":false', '"data_omitted":true'),
            text.replace('"version":1', '"version":true'),
            text.replace('"version":1', '"version":1,"extra":0'),
            text.replace('"data":{}', '"data":NaN'),
            text.replace('message.delivery_confirmation_timeout', 'message.created'),
            text.replace('"data":{}', '"data":"' + "x" * 8_192 + '"'),
        ):
            with self.assertRaises(frames.AuditError):
                frames.decode_frame(changed)


class NativeCliTransportTests(unittest.TestCase):
    def client(self, root, runner):
        image = root / "agentchat"
        image.write_bytes(b"owned fixture image; never executed by this unit test")
        return cli.FixtureCli(image=image, image_sha256=hashlib.sha256(image.read_bytes()).hexdigest(),
                              project_id=str(uuid.UUID(int=1)), project=root, state=root / "state",
                              config=root / "config", artifacts=root / "artifacts", runner=runner)

    def test_maximum_control_body_uses_stdin_and_exact_actor_without_home_substitution(self):
        calls = []
        def run(command, **options):
            calls.append((command, options))
            request = command[command.index("--request-id") + 1]
            envelope = dict(version=1, project_id=str(uuid.UUID(int=1)), request_id=request, result={})
            return subprocess.CompletedProcess(command, 0, json.dumps(envelope).encode(), b"")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            client = self.client(root, run)
            actor = cli.NativeActor("claude", "opaque native/chat ID")
            body = "\0" * 65_536
            client.post("post", actor, "native", body)
            command, options = calls[0]
            self.assertIn("--stdin", command)
            self.assertNotIn(body, command)
            self.assertNotIn("--context", command)
            self.assertEqual(options["input"], body.encode())
            self.assertEqual(options["env"].get("HOME"), os.environ.get("HOME"))
            self.assertEqual(options["env"].get("CODEX_HOME"), os.environ.get("CODEX_HOME"))
            with self.assertRaises(cli.CallError):
                client.whoami("unbound", None)
            self.assertEqual(len(calls), 1)

    def test_uncertain_mutation_keeps_original_request_and_cannot_be_replayed(self):
        calls = []
        def run(command, **options):
            calls.append(command)
            intent = json.loads((Path(options["cwd"]) / "artifacts/ack.json").read_text())
            self.assertEqual(intent["state"], "intent")
            self.assertTrue(intent["outcome_unknown"])
            raise subprocess.TimeoutExpired(command, 60, output=b"partial", stderr=b"uncertain")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            client = self.client(root, run)
            actor = cli.NativeActor("codex", "exact-root")
            request_id = str(uuid.UUID(int=2))
            for _ in range(2):
                with self.assertRaises(cli.CallError):
                    client.ack("ack", actor, str(uuid.UUID(int=3)), request_id=request_id)
            self.assertEqual(len(calls), 1)
            captured = json.loads((root / "artifacts/ack.json").read_text())
            self.assertTrue(captured["outcome_unknown"])
            self.assertEqual(captured["request_id"], request_id)


class NativePreparationTests(unittest.TestCase):
    def test_config_hash_baseline_is_secret_free_and_persisted_before_native_spawn(self):
        with mock.patch.object(actor_producer, "file_fact", side_effect=lambda path: dict(path=str(path), sha256="hash-only")) as fact:
            baseline = actor_producer.configuration_snapshot()
        self.assertEqual(len(fact.call_args_list), 4)
        self.assertFalse(any("auth" in str(call.args[0]) or "credential" in str(call.args[0]) for call in fact.call_args_list))
        with tempfile.TemporaryDirectory() as directory:
            artifacts = Path(directory) / "native"
            def not_spawned(*_args, **_kwargs):
                self.assertEqual(json.loads((artifacts / "pre-native-config-hashes.json").read_text()), baseline)
                raise OSError("offline test; no process started")
            with mock.patch.object(actor_producer.pty, "openpty", return_value=(101, 102)), \
                    mock.patch.object(actor_producer.fcntl, "ioctl"), mock.patch.object(actor_producer.os, "close"), \
                    mock.patch.object(actor_producer.subprocess, "Popen", side_effect=not_spawned):
                with self.assertRaises(OSError):
                    actor_producer.Terminal(["not-executed"], project=Path(directory), environment={},
                                           artifacts=artifacts, configuration_baseline=baseline)

    def test_ready_indices_collapse_only_the_same_complete_original_binding(self):
        project, generation = str(uuid.UUID(int=1)), str(uuid.UUID(int=2))
        proof = dict(owner=dict(pid=12), supervisor=dict(pid=11))
        value = dict(version=1, control="/owned/join.sock", preferred_name="receiver",
                     binding=dict(identity=dict(generation=generation, owner=json.dumps(proof)),
                                  context=dict(host="codex", project_id=project,
                                               working_directory="/fixture/project", host_session_id="native-root",
                                               context_id=str(uuid.UUID(int=3)))))
        rows = [(Path("/ready") / name, copy.deepcopy(value)) for name in
                (generation + ".json", "root-index.json", "scope-index.json")]
        args = dict(host="codex", project_id=project, project="/fixture/project", supervisor_pid=11,
                    observe=lambda _token: True)
        selected = actor_producer.readiness_aliases(rows, **args)
        self.assertEqual(selected[0].name, generation + ".json")
        self.assertEqual(selected[4], generation)
        self.assertEqual(selected[1].chat_id, "native-root")
        for field, wrong in (("control", "/foreign/join.sock"), ("preferred_name", "foreign")):
            changed = copy.deepcopy(rows)
            changed[-1][1][field] = wrong
            with self.assertRaises(cli.CallError):
                actor_producer.readiness_aliases(changed, **args)
        changed = copy.deepcopy(rows)
        changed[-1][1]["binding"]["context"]["context_id"] = str(uuid.UUID(int=4))
        with self.assertRaises(cli.CallError):
            actor_producer.readiness_aliases(changed, **args)
        self.assertIsNone(actor_producer.readiness_aliases(rows[1:], **args))
        self.assertEqual(actor_producer.readiness_aliases(rows, **args)[0], rows[0][0])
        # A missing canonical receipt cannot hide conflicting published proofs.
        changed = copy.deepcopy(rows[1:])
        changed[-1][1]["binding"]["identity"]["owner"] = json.dumps(
            dict(owner=dict(pid=99), supervisor=dict(pid=11)))
        with self.assertRaises(cli.CallError):
            actor_producer.readiness_aliases(changed, **args)

    def publication(self):
        actor = cli.NativeActor("codex", "native-root")
        original = dict(project_id=str(uuid.UUID(int=1)), context_id=str(uuid.UUID(int=2)),
                        host="codex", host_session_id=actor.chat_id, working_directory="/fixture/project",
                        host_version="observed", os_profile="observed-os", incarnation=str(uuid.UUID(int=3)))
        current = {**original, "context_id": str(uuid.UUID(int=4)), "incarnation": str(uuid.UUID(int=5))}
        proof = dict(owner=dict(pid=12), supervisor=dict(pid=11))
        generation = str(uuid.UUID(int=6))
        result = dict(project_id=original["project_id"], context_id=current["context_id"], participation="joined",
                      actor=dict(kind="agent", host="codex", chat_id=actor.chat_id,
                                 actor_id=str(uuid.UUID(int=7)), incarnation=current["incarnation"]),
                      native_delivery=dict(generation=generation))
        report = dict(context=current, launch=dict(generation=generation, owner=json.dumps(proof)),
                      agent_id=result["actor"]["actor_id"])
        return actor, original, current, proof, generation, result, report

    def test_current_descriptor_requires_public_selection_and_exact_original_owner_report(self):
        actor, original, current, proof, generation, result, report = self.publication()
        args = dict(actor=actor, original=original, proof=proof, generation=generation)
        self.assertEqual(actor_producer.published_context(result, [report], **args), current)
        self.assertIsNone(actor_producer.published_context(result, [], **args))
        for wrong in ({**report, "launch": {**report["launch"], "generation": str(uuid.UUID(int=8))}},
                      {**report, "launch": {**report["launch"], "owner": json.dumps(dict(owner=dict(pid=99), supervisor=dict(pid=11)))}},
                      {**report, "context": {**current, "host_session_id": "foreign-root"}},
                      {**report, "agent_id": str(uuid.UUID(int=9))}):
            with self.assertRaises(cli.CallError):
                actor_producer.published_context(result, [wrong], **args)
        with self.assertRaises(cli.CallError):
            actor_producer.published_context({**result, "actor": {**result["actor"], "chat_id": "foreign-root"}}, [report], **args)

    def test_public_descriptor_report_race_has_bounded_read_only_retries(self):
        actor, original, current, proof, generation, result, report = self.publication()
        with tempfile.TemporaryDirectory() as directory:
            producer = actor_producer.ActorProducer.__new__(actor_producer.ActorProducer)
            producer.actor, producer.context, producer.original_context = actor, original, original
            producer.proof, producer.launch_generation = proof, generation
            producer.preferred_name = None
            producer.artifacts = Path(directory)
            producer.readiness = lambda: True
            client = mock.Mock()
            calls = []
            def whoami(label, selected):
                calls.append((label, selected))
                if len(calls) == 2:
                    (producer.artifacts / "authorized.reattached.json").write_text(json.dumps(report))
                return dict(result=result)
            client.whoami.side_effect = whoami
            self.assertEqual(producer.selected_whoami(client, "selector"), result)
            self.assertEqual(producer.context, current)
            self.assertEqual([label for label, _actor in calls], ["selector-route-0", "selector-route-1"])
            self.assertEqual([call[0] for call in client.method_calls], ["whoami", "whoami"])

    def test_retained_selector_refuses_a_different_public_handle_without_mutations(self):
        actor, original, current, proof, generation, result, report = self.publication()
        with tempfile.TemporaryDirectory() as directory:
            producer = actor_producer.ActorProducer.__new__(actor_producer.ActorProducer)
            producer.actor, producer.context, producer.original_context = actor, original, original
            producer.proof, producer.launch_generation = proof, generation
            producer.preferred_name = "receipt-receiver-0123456789ab"
            producer.artifacts = Path(directory);producer.readiness = lambda: True
            (producer.artifacts / "authorized.reattached.json").write_text(json.dumps(report))
            result["actor"]["handle"] = producer.preferred_name
            client = mock.Mock();client.whoami.return_value = dict(result=result)
            self.assertEqual(producer.selected_whoami(client, "exact"), result)
            result["actor"]["handle"] = "receipt-receiver"
            with self.assertRaises(cli.CallError):
                producer.selected_whoami(client, "changed-handle")
            self.assertEqual([call[0] for call in client.method_calls], ["whoami", "whoami"])

    def test_initial_selector_waits_for_activation_report_without_retrying_refusals(self):
        campaign = run_profile.ProfileCampaign.__new__(run_profile.ProfileCampaign)
        campaign.stage = "sender-starting"
        campaign.setup = mock.Mock()
        campaign.actors = {name: mock.Mock() for name in ("receiver", "sender")}
        for producer in campaign.actors.values():
            producer.readiness.return_value = True
            producer.activation_reported.return_value = False
            producer.selected_whoami.side_effect = cli.CallError("SessionRequired before association")
        campaign.label = lambda name: name
        campaign.advance_startup()
        self.assertEqual(campaign.stage, "sender-starting")
        for producer in campaign.actors.values():
            producer.selected_whoami.assert_not_called()
            producer.activation_reported.return_value = True
        with self.assertRaises(cli.CallError):
            campaign.advance_startup()
        campaign.actors["receiver"].selected_whoami.assert_called_once()
        campaign.actors["sender"].selected_whoami.assert_not_called()

    def test_activation_report_requires_exact_original_owner_generation_and_root(self):
        actor, original, current, proof, generation, result, report = self.publication()
        report.update(shared_supervisor=True, membership_committed=True)
        with tempfile.TemporaryDirectory() as directory:
            producer = actor_producer.ActorProducer.__new__(actor_producer.ActorProducer)
            producer.actor, producer.context, producer.original_context = actor, original, original
            producer.proof, producer.launch_generation = proof, generation
            producer.artifacts = Path(directory);producer.readiness = lambda: True
            self.assertFalse(producer.activation_reported())
            (producer.artifacts / "authorized.json").write_text(json.dumps(report))
            self.assertFalse(producer.activation_reported())
            path = producer.artifacts / "authorized.activated.json"
            path.write_text(json.dumps(report));self.assertTrue(producer.activation_reported())
            self.assertEqual(producer.context, original)
            for wrong in ({**report, "launch": {**report["launch"], "generation": str(uuid.UUID(int=8))}},
                          {**report, "launch": {**report["launch"], "owner": json.dumps(dict(owner=dict(pid=99), supervisor=dict(pid=11)))}},
                          {**report, "context": {**current, "host_session_id": "foreign-root"}},
                          {**report, "context": {**current, "project_id": str(uuid.UUID(int=9))}},
                          {**report, "membership_committed": False},
                          {**report, "shared_supervisor": False}):
                path.write_text(json.dumps(wrong))
                with self.assertRaises(cli.CallError):
                    producer.activation_reported()

    def test_passive_codex_originals_use_only_native_api_rollout_and_exact_session(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory);path = root / "rollout-observed.jsonl"
            body = "\0日本語😀\r\n--\n[/AgentChat:Message]"
            original = message(body)
            rows = [dict(type="session_meta", payload=dict(id="native-root")),
                    dict(type="response_item", payload=dict(type="message", role="assistant",
                         content=[dict(type="input_text", text="not original")])),
                    dict(type="response_item", payload=dict(type="message", role="user",
                         content=[dict(type="input_text", text=original)]))]
            raw = ("\n".join(json.dumps(row, ensure_ascii=False) for row in rows) + "\n").encode()
            path.write_bytes(raw)
            producer = mock.Mock(host="codex", actor=cli.NativeActor("codex", "native-root"),
                                 context=dict(context_id="current"), proof=dict(owner=dict(pid=12), supervisor=dict(pid=11)))
            producer.readiness.return_value = True
            producer.selected_whoami.return_value = dict(context_id="current")
            observation = dict(root="native-root", context_id="current", owner=producer.proof["owner"],
                               raw=dict(thread=dict(id="native-root", path=str(path), turns=[])))
            observer = native_observer.NativeObserver(producer, artifacts=root / "observed")
            with mock.patch.object(native_observer, "matches", return_value=True):
                actual = observer.codex_rows("passive", observation, client=mock.Mock())
            self.assertEqual(native_observer.codex_rollout_texts(actual), [original])
            self.assertEqual(frames.decode_frame(native_observer.codex_rollout_texts(actual)[0]).body, body)
            self.assertEqual((root / "observed/passive.jsonl").read_bytes(), raw)
            self.assertEqual(native_observer.user_texts("codex", observation["raw"]), [])
            for field, value in (("context_id", "stale"), ("root", "foreign"), ("owner", dict(pid=99))):
                wrong = {**observation, field: value}
                with self.assertRaises(cli.CallError):observer.codex_rows("wrong", wrong, client=mock.Mock())
            rows[0]["payload"]["id"] = "foreign-root";path.write_text("\n".join(json.dumps(row) for row in rows))
            with mock.patch.object(native_observer, "matches", return_value=True):
                with self.assertRaises(cli.CallError):observer.codex_rows("foreign-session", observation, client=mock.Mock())
            self.assertFalse((root / "observed/foreign-session.jsonl").exists())
            rows[0]["payload"]["id"] = "native-root"
            path.write_text("\n".join(json.dumps(row) for row in rows))
            with mock.patch.object(native_observer, "matches", return_value=False):
                with self.assertRaises(cli.CallError):observer.codex_rows("lost-owner", observation, client=mock.Mock())
            producer.selected_whoami.side_effect = [dict(context_id="current"), dict(context_id="replacement")]
            with mock.patch.object(native_observer, "matches", return_value=True):
                with self.assertRaises(cli.CallError):observer.codex_rows("changed-descriptor", observation, client=mock.Mock())
            self.assertFalse((root / "observed/changed-descriptor.jsonl").exists())

    def test_codex_rollout_refuses_auth_paths_symlinks_size_and_unstable_observation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory);path = root / "rollout-observed.jsonl";path.write_bytes(b"{}\n")
            link = root / "rollout-link.jsonl";link.symlink_to(path)
            with self.assertRaises(cli.CallError):native_observer.rollout_bytes(link)
            with path.open("wb") as f:f.truncate(16 * 1024 * 1024 + 1)
            with self.assertRaises(cli.CallError):native_observer.rollout_bytes(path)
            producer = mock.Mock(host="codex", actor=cli.NativeActor("codex", "native-root"),
                                 context=dict(context_id="current"), proof=dict(owner=dict(pid=12), supervisor=dict(pid=11)))
            producer.readiness.return_value = True
            producer.selected_whoami.return_value = dict(context_id="current")
            observation = dict(root="native-root", context_id="current", owner=producer.proof["owner"],
                               raw=dict(thread=dict(id="native-root", path=str(root / "auth.json"), turns=[])))
            observer = native_observer.NativeObserver(producer, artifacts=root / "observed")
            with mock.patch.object(native_observer, "rollout_bytes") as read:
                with self.assertRaises(cli.CallError):observer.codex_rows("no-auth", observation, client=mock.Mock())
                read.assert_not_called()
            path.write_bytes(b"{}\n");before = path.stat()
            with mock.patch.object(Path, "lstat", side_effect=[before, mock.Mock(st_dev=before.st_dev,
                    st_ino=before.st_ino, st_mtime_ns=before.st_mtime_ns + 1, st_size=before.st_size)]):
                with self.assertRaises(cli.CallError):native_observer.rollout_bytes(path)

    def test_normal_owned_exit_is_separate_from_native_approval_decisions(self):
        import threading
        with tempfile.TemporaryDirectory() as directory:
            terminal = actor_producer.Terminal.__new__(actor_producer.Terminal)
            terminal.artifacts = Path(directory)
            terminal.birth = dict(pid=12)
            terminal.child = mock.Mock()
            terminal.child.poll.return_value = None
            terminal.lock = threading.Lock()
            terminal.master = 123
            with mock.patch.object(actor_producer, "matches", return_value=True), mock.patch.object(actor_producer.os, "write") as write, mock.patch.object(actor_producer.time, "sleep"):
                terminal.finish_native("normal-finish", "codex")
                self.assertEqual([call.args[1] for call in write.call_args_list], [b"/quit", b"\r"])
                record = json.loads((terminal.artifacts / "normal-finish.json").read_text())
                self.assertFalse(record["native_approval_decision"])
                with self.assertRaises(cli.CallError):
                    terminal.decision(label="must-not-approve", terminal_sha256="none", key="enter",
                                      authorization="owned_fixture_exit")
                self.assertEqual(write.call_count, 2)
            terminal.child.kill.assert_not_called()
            terminal.child.terminate.assert_not_called()

    def test_explicit_native_navigation_does_not_confirm_the_selected_action(self):
        import threading
        with tempfile.TemporaryDirectory() as directory:
            terminal = actor_producer.Terminal.__new__(actor_producer.Terminal)
            terminal.artifacts = Path(directory)
            terminal.raw = Path(directory) / "terminal.raw"
            terminal.raw.write_bytes(b"No, exit\nYes, I trust this folder\n")
            terminal.birth, terminal.master = dict(pid=12), 123
            terminal.lock = threading.Lock()
            digest = hashlib.sha256(terminal.raw.read_bytes()).hexdigest()
            with mock.patch.object(actor_producer, "matches", return_value=True), mock.patch.object(actor_producer.os, "write") as write:
                for key in ("down", "up"):
                    terminal.decision(label=key, terminal_sha256=digest, key=key,
                                      authorization="explicit_user_reply")
                self.assertEqual([call.args[1] for call in write.call_args_list], [b"\x1b[B", b"\x1b[A"])
                self.assertNotIn(b"\r", [call.args[1] for call in write.call_args_list])
                self.assertEqual(json.loads((terminal.artifacts / "down.json").read_text())["key"], "down")

    def test_native_navigation_retains_exact_human_screen_and_owner_guards(self):
        import threading
        with tempfile.TemporaryDirectory() as directory:
            terminal = actor_producer.Terminal.__new__(actor_producer.Terminal)
            terminal.artifacts = Path(directory)
            terminal.raw = Path(directory) / "terminal.raw"
            terminal.raw.write_bytes(b"Actual normal host decision")
            terminal.birth, terminal.master = dict(pid=12), 123
            terminal.lock = threading.Lock()
            digest = hashlib.sha256(terminal.raw.read_bytes()).hexdigest()
            for authorization, screen, current, key in (
                    ("owned_fixture_exit", digest, True, "down"),
                    ("explicit_user_reply", "changed", True, "down"),
                    ("explicit_user_reply", digest, False, "down"),
                    ("explicit_user_reply", digest, True, "arbitrary-keys")):
                with mock.patch.object(actor_producer, "matches", return_value=current), mock.patch.object(actor_producer.os, "write") as write:
                    with self.assertRaises(cli.CallError):
                        terminal.decision(label="refused", terminal_sha256=screen, key=key,
                                          authorization=authorization)
                    write.assert_not_called()
                    self.assertFalse((terminal.artifacts / "refused.json").exists())

    def test_cosmetic_theme_cannot_answer_auth_trust_or_permission(self):
        import threading
        with tempfile.TemporaryDirectory() as directory:
            terminal = actor_producer.Terminal.__new__(actor_producer.Terminal)
            terminal.artifacts = Path(directory)
            terminal.raw = Path(directory) / "terminal.raw"
            terminal.birth, terminal.master = dict(pid=12), 123
            terminal.child, terminal.lock = mock.Mock(), threading.Lock()
            terminal.child.poll.return_value = None
            theme = (b"Choose\x1b[9Gthe text style that looks best with your terminal\n"
                     b"/theme\nDark\x1b[11Gmode")
            terminal.raw.write_bytes(theme)
            with mock.patch.object(actor_producer, "matches", return_value=True), mock.patch.object(actor_producer.os, "write") as write:
                terminal.cosmetic_theme(label="theme", terminal_sha256=hashlib.sha256(theme).hexdigest())
                self.assertEqual([call.args[1] for call in write.call_args_list], [b"\r"])
                record = json.loads((terminal.artifacts / "theme.json").read_text())
                self.assertFalse(record["native_approval_decision"])
                for extra in (b"login", b"Trust folder?", b"Permission request", b"OAuth"):
                    raw = theme + extra
                    terminal.raw.write_bytes(raw)
                    with self.assertRaises(cli.CallError):
                        terminal.cosmetic_theme(label="denied", terminal_sha256=hashlib.sha256(raw).hexdigest())
                terminal.raw.write_bytes(theme)
                with self.assertRaises(cli.CallError):
                    terminal.cosmetic_theme(label="changed", terminal_sha256="changed")
                self.assertEqual(write.call_count, 1)


    def test_runtime_ready_uses_observed_short_socket_with_exact_namespace_and_owner(self):
        # Fixed synthetic namespace/hash, independent of this fixture's endpoint
        # calculation and of any workstation directory or temporary-path alias.
        project = "06afbac4-0278-4aed-8d60-36d181ea4b05"
        state = Path("/agentchat-native-case/state")
        socket = Path(f"/tmp/agentchat-{os.geteuid()}-8d0af07dd84db36bc802b9c8a8649e15/rpc.sock")
        value = dict(candidate_ready=True, admitted=False, socket=str(socket),
                     instance="e3c69de7-4341-444a-94d2-bede2d253043")
        def metadata(path):
            kind, mode = ((stat.S_IFSOCK, 0o600) if path == socket else
                          (stat.S_IFDIR, 0o700) if path == socket.parent else (stat.S_IFREG, 0o600))
            return type("ObservedMetadata", (), dict(st_mode=kind | mode, st_uid=os.geteuid(), st_dev=1, st_ino=2))()
        args = dict(state=state, project_id=project, birth=dict(pid=15582),
                    observe=lambda _token: True, metadata=metadata)
        endpoint = fixture_setup.runtime_endpoint(value, **args)
        self.assertEqual(endpoint["socket"], str(socket))
        self.assertNotIn(str(state), endpoint["socket"])
        for changed in ({**value, "socket": str(state / "rpc.sock")},
                        {**value, "socket": "/tmp/foreign/rpc.sock"}, {**value, "instance": str(uuid.UUID(int=0))}):
            with self.assertRaises((cli.CallError, frames.AuditError)):
                fixture_setup.runtime_endpoint(changed, **args)
        with self.assertRaises(cli.CallError):
            fixture_setup.runtime_endpoint(value, **{**args, "observe": lambda _token: False})
        for wrong in (dict(st_mode=stat.S_IFLNK | 0o600, st_uid=os.geteuid()),
                      dict(st_mode=stat.S_IFSOCK | 0o666, st_uid=os.geteuid()),
                      dict(st_mode=stat.S_IFSOCK | 0o600, st_uid=os.geteuid() + 1)):
            with self.assertRaises(cli.CallError):
                fixture_setup.runtime_endpoint(value, **{**args, "metadata": lambda selected:
                    type("Wrong", (), wrong)() if selected == socket else metadata(selected)})

    def test_runtime_ready_requires_the_actual_actor_free_handshake_instance(self):
        for health in (dict(reachable=True, instance_id=str(uuid.UUID(int=2))),
                       dict(reachable=False, instance_id=str(uuid.UUID(int=1))),
                       dict(reachable=True, instance_id=str(uuid.UUID(int=1)))):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                setup = fixture_setup.FixtureSetup(mock.Mock())
                setup.plan.artifacts = root
                setup.plan.state = root / "state"
                setup.runtime = mock.Mock()
                setup.runtime.label = "runtime"
                setup.runtime.birth = dict(pid=12)
                setup.runtime.child.poll.return_value = None
                setup.client = mock.Mock()
                setup.client.project_id = str(uuid.UUID(int=1))
                setup.client._call.return_value = dict(result=dict(runtime=health))
                (root / "runtime.log").write_text(json.dumps(dict(candidate_ready=True))+'\n')
                with mock.patch.object(fixture_setup, "runtime_endpoint", return_value=dict(instance_id=str(uuid.UUID(int=1)))), mock.patch.object(fixture_setup, "matches", return_value=True):
                    if health["reachable"] and health["instance_id"] == str(uuid.UUID(int=1)):
                        self.assertTrue(setup.poll_runtime_ready())
                        self.assertTrue((root / "runtime-ready.json").is_file())
                    else:
                        with self.assertRaises(cli.CallError):
                            setup.poll_runtime_ready()
                        self.assertFalse(setup.runtime_ready)
                self.assertEqual(setup.client._call.call_args.args[1], ["doctor"])


    def test_readiness_requires_exact_project_root_and_original_physical_producer(self):
        identifier = str(uuid.UUID(int=1))
        proof = dict(owner=dict(pid=12), supervisor=dict(pid=11))
        value = dict(version=1, binding=dict(
            identity=dict(owner=json.dumps(proof)),
            context=dict(host="codex", project_id=identifier, working_directory="/fixture/project",
                         host_session_id="opaque/native-root", context_id=identifier)))
        arguments = dict(host="codex", project_id=identifier, project="/fixture/project",
                         supervisor_pid=11, observe=lambda _token: True)
        actor, context, actual = actor_producer.correlate_ready(value, **arguments)
        self.assertEqual(actor.chat_id, "opaque/native-root")
        self.assertEqual(actual, proof)
        for key, wrong in (("host", "claude"), ("project_id", str(uuid.UUID(int=2))),
                           ("project", "/other/project"), ("supervisor_pid", 12)):
            with self.assertRaises(cli.CallError):
                actor_producer.correlate_ready(value, **{**arguments, key: wrong})
        with self.assertRaises(cli.CallError):
            actor_producer.correlate_ready(value, **{**arguments, "observe": lambda _token: False})

    def test_profile_probe_retains_no_environment_and_failed_enumeration_is_unknown(self):
        profile = "/fixture/profile"
        def run(_command, **_options):
            return subprocess.CompletedProcess([], 0,
                "7 native TOKEN=do-not-retain CLAUDE_CONFIG_DIR=/fixture/profile\n8 other SECRET=x\n", "")
        observed = resource_probe.profile_processes(profile, runner=run)
        self.assertEqual(observed["state"], "busy")
        self.assertEqual(observed["pids"], [7])
        self.assertNotIn("TOKEN", json.dumps(observed))
        failed = lambda *_args, **_options: subprocess.CompletedProcess([], 1, "", "not readable")
        self.assertEqual(resource_probe.profile_processes(profile, runner=failed)["state"], "unknown")

    def test_fixture_inputs_keep_containment_and_refuse_existing_case_or_changed_image(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image = root / "image"
            image.write_bytes(b"not executed")
            frozen = actor_producer.Image(image, hashlib.sha256(image.read_bytes()).hexdigest())
            plan = fixture_plan.FixturePlan("claude", root / "case", frozen, frozen, frozen, "observed", frozen)
            plan.validate()
            self.assertEqual(plan.profile, plan.config / "host-config")
            environment = plan.environment()
            self.assertEqual(environment.get("HOME"), os.environ.get("HOME"))
            self.assertEqual(environment.get("CODEX_HOME"), os.environ.get("CODEX_HOME"))
            self.assertEqual(environment["CLAUDE_CONFIG_DIR"], str(plan.profile))
            plan.case.mkdir()
            with self.assertRaises(cli.CallError):
                plan.validate()
            image.write_bytes(b"different source image")
            with self.assertRaises(cli.CallError):
                frozen.check()

    def test_configuration_preimages_refuse_secrets_and_uncertain_or_foreign_restoration(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            profile = root / "profile"
            profile.mkdir()
            settings = profile / "settings.json"
            settings.write_text('{"accessToken":"do-not-copy"}')
            selected = preservation.SelectedConfiguration(profile, root / "before")
            quiet = mock.patch.object(preservation, "profile_processes", return_value=dict(state="observed_quiet"))
            with quiet:
                with self.assertRaises(cli.CallError):
                    selected.capture()
                self.assertFalse(selected.artifacts.exists())
                original = b'{"theme":"original"}\n'
                settings.write_bytes(original)
                credentials = profile / ".credentials.json"
                credentials.write_bytes(b"never read or copied")
                selected.capture()
                changed = b'{"theme":"native-change"}\n'
                settings.write_bytes(changed)
                expected = {"settings.json": hashlib.sha256(changed).hexdigest(), ".claude.json": None}
                with self.assertRaises(cli.CallError):
                    selected.restore(owners_gone=False, expected_current=expected)
                with self.assertRaises(cli.CallError):
                    selected.restore(owners_gone=True, expected_current={**expected, "settings.json": "foreign"})
                self.assertEqual(settings.read_bytes(), changed)
                selected.restore(owners_gone=True, expected_current=expected)
                self.assertEqual(settings.read_bytes(), original)
                self.assertEqual(credentials.read_bytes(), b"never read or copied")
                self.assertFalse((selected.artifacts / ".credentials.json").exists())

    def test_actual_user_text_extraction_excludes_assistant_previews(self):
        exact = message("complete original")
        codex = dict(thread=dict(turns=[dict(items=[dict(type="userMessage", content=[dict(type="text", text=exact)]),
                                                   dict(type="agentMessage", text="preview")])]))
        claude = [dict(type="user", message=dict(content=exact)),
                  dict(type="assistant", message=dict(content="preview"))]
        self.assertEqual(native_observer.user_texts("codex", codex), [exact])
        self.assertEqual(native_observer.user_texts("claude", claude), [exact])

    def test_fresh_setup_uses_contained_profile_and_never_executes_a_native_image(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image = root / "not-an-executable"
            image.write_bytes(b"offline fixture only")
            frozen = actor_producer.Image(image, hashlib.sha256(image.read_bytes()).hexdigest())
            plan = fixture_plan.FixturePlan("claude", root / "case", frozen, frozen, frozen, "observed", frozen)
            calls = []
            def run(command, **options):
                calls.append((command, options))
                if "--request-id" in command:
                    value = dict(version=1, request_id=command[command.index("--request-id") + 1],
                                 project_id=str(uuid.UUID(int=1)), result={})
                elif "launch-plan" in command:
                    value = dict(settings=str(plan.profile / "settings.json"),
                                 registry=str(plan.profile / ".claude.json"), plugin=str(plan.profile / "skills"))
                else:
                    value = {}
                return subprocess.CompletedProcess(command, 0, json.dumps(value).encode(), b"")
            setup = fixture_setup.FixtureSetup(plan, runner=run)
            self.assertEqual(setup.prepare(), str(uuid.UUID(int=1)))
            self.assertEqual(len(calls), 7)
            self.assertEqual([command[-1] for command, _options in calls[:3]], ["--version", "--help", "--help"])
            self.assertTrue(all(options["timeout"] == 300 for _command, options in calls[:3]))
            self.assertIn("native", calls[4][0])
            self.assertEqual(calls[4][1]["timeout"], 120)
            for command, options in calls:
                self.assertNotIn("--dangerously-skip-permissions", command)
                self.assertEqual(options["env"]["CLAUDE_CONFIG_DIR"], str(plan.profile))
                self.assertEqual(options["env"].get("HOME"), os.environ.get("HOME"))
            with self.assertRaises(cli.CallError):
                setup.start_setup_observer()
            with self.assertRaises(cli.CallError):
                setup.start_producer("receiver")
            self.assertIsNone(setup.runtime)
            self.assertIsNone(setup.viewer)

    def test_profile_entry_requires_full_frozen_inputs_and_never_replays_a_failed_command(self):
        with self.assertRaises(cli.CallError):
            run_profile.load_plan(dict(host="claude", case="/fresh"))
        template = json.loads(path.with_name("inputs.example.json").read_text())
        codex = {**template, "host": "codex"}
        with self.assertRaises(cli.CallError):
            run_profile.load_plan(codex)
        plan = run_profile.load_plan(template)
        with self.assertRaises((cli.CallError, OSError)):
            plan.validate()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "commands").mkdir()
            (root / "results").mkdir()
            command = root / "commands/001.json"
            command.write_text('{"kind":"post","body":"only once"}')
            campaign = run_profile.ProfileCampaign.__new__(run_profile.ProfileCampaign)
            campaign.setup = mock.Mock()
            campaign.setup.plan.case = root
            campaign.dispatch = mock.Mock(side_effect=cli.CallError("uncertain original mutation"))
            campaign.commands()
            campaign.commands()
            self.assertEqual(campaign.dispatch.call_count, 1)
            retained = json.loads((root / "results/001.json").read_text())
            self.assertEqual(retained["state"], "failed")
            self.assertTrue(retained["outcome_unknown"])

    def test_preparation_timeout_keeps_original_partial_output_and_intent(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            setup = fixture_setup.FixtureSetup(mock.Mock())
            setup.plan.artifacts = root
            setup.plan.project = root
            setup.plan.environment.return_value = os.environ.copy()
            setup.runner = mock.Mock(side_effect=subprocess.TimeoutExpired([], 120, output=b"original partial", stderr=b"original error"))
            with self.assertRaises(cli.CallError):
                setup._run("one-install", ["not-executed"])
            self.assertEqual((root / "one-install.stdout").read_bytes(), b"original partial")
            self.assertEqual((root / "one-install.stderr").read_bytes(), b"original error")
            self.assertTrue(json.loads((root / "one-install.json").read_text())["outcome_unknown"])
            with self.assertRaises(FileExistsError):
                setup._run("one-install", ["must-not-repeat"])
            self.assertEqual(setup.runner.call_count, 1)


class RetainedFixtureTests(unittest.TestCase):
    def retained(self, root):
        original = root / "original"
        for directory in (original / "project/.codex", original / "state", original / "config", original / "artifacts"):
            directory.mkdir(parents=True, mode=0o700, exist_ok=True)
        images = {}
        for name in ("cli", "helper", "native", "source_seal"):
            path = root / (name + ".fake")
            path.write_text(name + " offline input; never executed")
            images[name] = actor_producer.Image(path, hashlib.sha256(path.read_bytes()).hexdigest())
        request, project_id = str(uuid.uuid4()), str(uuid.uuid4())
        fixture = dict(host="codex", case=str(original), project=str(original / "project"),
                       state=str(original / "state"), config=str(original / "config"), native_version="observed",
                       images={name: dict(path=str(value.path), sha256=value.digest) for name, value in images.items()})
        values = {"fixture-inputs.json": fixture,
                  "project-install.json": dict(state="returned", exit_code=0, outcome_unknown=False,
                      command=["offline", "--request-id", request]),
                  "project-install.stdout": dict(version=1, result={}, request_id=request, project_id=project_id),
                  "runtime-terminal.json": dict(exit_code=0, scope="original owned child only"),
                  "startup-failed.json": dict(error="offline pre-readiness failure"),
                  "postmortem-event-summary.json": dict(project_id=project_id, events=dict(
                      head="2", scan_cursor="2", next_cursor=None, items=[
                          dict(seq="1", kind="channel.created"), dict(seq="2", kind="project.channel_policy_materialized")])),
                  "native-terminal-proof.json": dict(original=dict(version=1,
                      owner=dict(pid=900001, uid=os.geteuid()), supervisor=dict(pid=900002, uid=os.geteuid())),
                      terminal=dict(owner=dict(esrch=True), supervisor=dict(esrch=True)))}
        for name, value in values.items():
            (original / "artifacts" / name).write_text(json.dumps(value))
        mcp = original / "project/.codex/config.toml"
        mcp.write_bytes(retained_setup.mcp_document(images["cli"].path, original / "state",
                        original / "config", images["native"].path, "observed"))
        approval = root / "approval.json"
        approval.write_text(json.dumps(dict(authorization="explicit_user_reply", project=str(original / "project"),
            retain_project_trust=True, fixture_mcp_final_image_update=True,
            fixture_update_review_reference="offline-fixture-review-only", user_reply_reference="offline-fixture-only")))
        trust = root / "trust-terminal.json"
        trust.write_text(json.dumps(dict(exit_code=0, original_pid_esrch=True, project=str(original / "project"),
            original_birth=dict(pid=900003, uid=os.geteuid()), native_model_prompt_sent=False,
            non_target_semantics_unchanged=True, target_entry_retained=dict(trust_level="trusted"),
            user_config_after_sha256="separate-approved-trust-hash")))
        hashes = {name: hashlib.sha256((original / name).read_bytes()).hexdigest() for name in retained_setup.RETAINED_FILES}
        plan = retained_setup.RetainedCodexPlan(host="codex", case=root / "new-attempt", native_version="observed",
            original_case=original, fixture_root=original, original_hashes=hashes, approval_record=approval,
            trust_terminal=actor_producer.Image(trust, hashlib.sha256(trust.read_bytes()).hexdigest()), **images)
        observed = dict(project=str(plan.project), trust_level="trusted", project_layer_enabled=True,
                        user_config_sha256="separate-approved-trust-hash")
        return plan, observed, project_id

    def test_preferred_names_are_case_role_specific_bounded_and_reject_history_collisions(self):
        case = Path("/owned/new-attempt")
        history = [dict(kind="agent.joined", data=dict(handle=name))
                   for name in ("receipt-receiver", "receipt-sender")]
        names = retained_setup.preferred_names(case, history)
        self.assertEqual(names, retained_setup.preferred_names(case, history))
        self.assertNotEqual(names, retained_setup.preferred_names(Path("/owned/next-attempt"), history))
        self.assertEqual(len(set(names.values())), 2)
        for role, name in names.items():
            self.assertLessEqual(len(name.encode()), 32)
            self.assertRegex(name, "^" + role + r"-[0-9a-f]{12}$")
            with self.assertRaises(cli.CallError):
                retained_setup.preferred_names(case, history + [dict(kind="agent.joined", data=dict(handle=name))])
        with tempfile.TemporaryDirectory() as directory:
            plan, observed, _project_id = self.retained(Path(directory).resolve())
            chosen = retained_setup.preferred_names(plan.case, [])["receipt-receiver"]
            path = plan.evidence_path("artifacts/postmortem-event-summary.json")
            page = json.loads(path.read_text());page["events"].update(head="3", scan_cursor="3")
            page["events"]["items"].append(dict(seq="3", kind="agent.joined", data=dict(handle=chosen)))
            path.write_text(json.dumps(page));plan.original_hashes["artifacts/postmortem-event-summary.json"] = hashlib.sha256(path.read_bytes()).hexdigest()
            runner = mock.Mock()
            with mock.patch.object(retained_setup.os, "kill", side_effect=ProcessLookupError), \
                    mock.patch.object(retained_setup, "retained_processes"):
                with self.assertRaises(cli.CallError):
                    retained_setup.RetainedCodexSetup(plan, trust_observation=lambda: observed, runner=runner).prepare()
            self.assertFalse(plan.case.exists())
            self.assertFalse(runner.called)

    def test_retained_launch_uses_saved_preferred_names_without_changing_artifact_roles(self):
        with tempfile.TemporaryDirectory() as directory:
            plan, observed, _project_id = self.retained(Path(directory).resolve())
            runner = mock.Mock(return_value=subprocess.CompletedProcess([], 0, stdout=b"offline help", stderr=b""))
            with mock.patch.object(retained_setup.os, "kill", side_effect=ProcessLookupError), \
                    mock.patch.object(retained_setup, "retained_processes"):
                setup = retained_setup.RetainedCodexSetup(plan, trust_observation=lambda: observed, runner=runner)
                setup.prepare()
            setup.runtime_ready = True
            setup.runtime = mock.Mock();setup.runtime.child.poll.return_value = None
            with mock.patch.object(actor_producer, "Terminal") as terminal, \
                    mock.patch.object(actor_producer, "configuration_snapshot", return_value={}):
                for role in ("receipt-receiver", "receipt-sender"):
                    producer = setup.start_producer(role)
                    self.assertEqual(producer.name, role)
                    self.assertEqual(producer.preferred_name, setup.preferred_names[role])
                    self.assertEqual(producer.artifacts, plan.artifacts / role)
                    command = terminal.call_args.args[0]
                    self.assertEqual(command[command.index("--name") + 1], setup.preferred_names[role])
                    self.assertIn(str(plan.artifacts / role / "authorized.json"), command)
                saved = json.loads((plan.artifacts / "native-preferred-names.json").read_text())
                self.assertEqual(saved["roles"], setup.preferred_names)
                saved["roles"]["receipt-receiver"] = "foreign-handle"
                (plan.artifacts / "native-preferred-names.json").write_text(json.dumps(saved))
                with self.assertRaises(cli.CallError):
                    setup.start_producer("receipt-receiver")
                self.assertEqual(terminal.call_count, 2)

    def test_same_path_preparation_preserves_install_and_uses_only_new_image_artifacts(self):
        with tempfile.TemporaryDirectory() as directory:
            plan, observed, project_id = self.retained(Path(directory).resolve())
            original = {name: (plan.original_case / name).read_bytes() for name in retained_setup.RETAINED_FILES}
            runner = mock.Mock(return_value=subprocess.CompletedProcess([], 0, stdout=b"offline help", stderr=b""))
            with mock.patch.object(retained_setup.os, "kill", side_effect=ProcessLookupError), \
                    mock.patch.object(retained_setup, "retained_processes"), mock.patch("subprocess.Popen") as spawn:
                setup = retained_setup.RetainedCodexSetup(plan, trust_observation=lambda: observed, runner=runner)
                self.assertEqual(setup.prepare(), project_id)
                self.assertFalse(spawn.called)
                with self.assertRaises(cli.CallError):
                    setup.prepare()
            self.assertEqual(len(runner.call_args_list), 3)
            self.assertEqual([call.args[0][-1] for call in runner.call_args_list], ["--version", "--help", "--help"])
            self.assertTrue(all(call.kwargs["cwd"] == plan.project for call in runner.call_args_list))
            self.assertEqual(setup.client.project_id, project_id)
            self.assertEqual(setup.client.environment["AGENTCHAT_HOME"], str(plan.state))
            self.assertEqual(setup.plan.cli.path.parent, plan.case / "images")
            self.assertEqual((plan.case / "artifacts/original-project-mcp.toml").read_bytes(), original["project/.codex/config.toml"])
            for name, raw in original.items():
                if name != "project/.codex/config.toml":
                    self.assertEqual((plan.original_case / name).read_bytes(), raw)
            self.assertEqual((plan.project / ".codex/config.toml").read_bytes(), retained_setup.mcp_document(
                setup.plan.cli.path, plan.state, plan.config, plan.native.path, plan.native_version))

    def test_same_path_requires_exact_approval_terminal_and_unchanged_originals(self):
        for failure in ("approval", "live-owner", "unknown-install", "foreign-mcp", "trust-pending"):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                plan, observed, _project_id = self.retained(Path(directory).resolve())
                if failure == "approval":
                    plan.approval_record.write_text(json.dumps(dict(authorization="not-approved")))
                if failure == "unknown-install":
                    path = plan.original_case / "artifacts/project-install.json"
                    value = json.loads(path.read_text());value["outcome_unknown"] = True;path.write_text(json.dumps(value))
                    plan.original_hashes["artifacts/project-install.json"] = hashlib.sha256(path.read_bytes()).hexdigest()
                if failure == "foreign-mcp":
                    path = plan.project / ".codex/config.toml";path.write_text("foreign extra config")
                    plan.original_hashes["project/.codex/config.toml"] = hashlib.sha256(path.read_bytes()).hexdigest()
                if failure == "trust-pending":
                    observed["project_layer_enabled"] = False
                runner = mock.Mock()
                with mock.patch.object(retained_setup.os, "kill", side_effect=None if failure == "live-owner" else ProcessLookupError), \
                        mock.patch.object(retained_setup, "retained_processes"):
                    with self.assertRaises(cli.CallError):
                        retained_setup.RetainedCodexSetup(plan, trust_observation=lambda: observed, runner=runner).prepare()
                self.assertFalse(plan.case.exists())
                self.assertFalse(runner.called)

    def test_same_path_does_not_overwrite_a_fixture_change_during_preflight(self):
        with tempfile.TemporaryDirectory() as directory:
            plan, observed, _project_id = self.retained(Path(directory).resolve())
            def changed(*_args, **_kwargs):
                (plan.project / ".codex/config.toml").write_bytes(b"unexpected later writer")
                return subprocess.CompletedProcess([], 0, stdout=b"", stderr=b"")
            with mock.patch.object(retained_setup.os, "kill", side_effect=ProcessLookupError), \
                    mock.patch.object(retained_setup, "retained_processes"):
                with self.assertRaises(cli.CallError):
                    retained_setup.RetainedCodexSetup(plan, trust_observation=lambda: observed, runner=changed).prepare()
            self.assertEqual((plan.project / ".codex/config.toml").read_bytes(), b"unexpected later writer")

    def test_native_trust_snapshot_is_read_only_and_retains_no_full_config(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve();plan, _observed, _project_id = self.retained(root)
            path = root / "user-config.toml";path.write_text("offline config")
            image = actor_producer.Image(path, hashlib.sha256(path.read_bytes()).hexdigest())
            response = dict(config=dict(secret="must-not-retain", projects={str(plan.project): dict(trust_level="trusted")}),
                layers=[dict(name=dict(type="project", dotCodexFolder=str(plan.project / ".codex")), disabledReason=None),
                        dict(name=dict(type="user", file=str(path)), disabledReason=None)])
            native = mock.Mock();native.request.return_value = response
            with mock.patch.object(retained_setup, "Codex", return_value=native):
                result = retained_setup.native_trust_snapshot(plan, "existing-socket", image)
                self.assertNotIn("must-not-retain", json.dumps(result))
                native.request.assert_called_once_with("config/read", dict(cwd=str(plan.project), includeLayers=True))
                self.assertTrue(native.close.called)
                response["layers"][0]["disabledReason"] = "normal trust still required"
                with self.assertRaises(cli.CallError):
                    retained_setup.native_trust_snapshot(plan, "existing-socket", image)

    def test_retained_quiescence_never_treats_failed_enumeration_as_quiet(self):
        for value in (subprocess.CompletedProcess([], 1, stdout="", stderr="denied"),
                      subprocess.CompletedProcess([], 0, stdout="42 agentchat AGENTCHAT_HOME=/exact-retained/state secret=unretained", stderr="")):
            with mock.patch.object(retained_setup.subprocess, "run", return_value=value):
                with self.assertRaises(cli.CallError) as error:
                    retained_setup.retained_processes(Path("/exact-retained"))
                self.assertNotIn("unretained", str(error.exception))

    def test_next_startup_attempt_retains_the_original_namespace_and_install_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve();plan, observed, project_id = self.retained(root)
            install = {name: plan.evidence_path(name).read_bytes() for name in (
                "artifacts/project-install.json", "artifacts/project-install.stdout")}
            sentinel = plan.state / "history-sentinel";sentinel.write_bytes(b"original history")
            runner = mock.Mock(return_value=subprocess.CompletedProcess([], 0, stdout=b"offline help", stderr=b""))
            with mock.patch.object(retained_setup.os, "kill", side_effect=ProcessLookupError), \
                    mock.patch.object(retained_setup, "retained_processes"):
                first = retained_setup.RetainedCodexSetup(plan, trust_observation=lambda: observed, runner=runner)
                first.prepare()
                previous = first.plan.case
                for name in ("startup-failed.json", "postmortem-event-summary.json", "runtime-terminal.json"):
                    (previous / "artifacts" / name).write_bytes((plan.original_case / "artifacts" / name).read_bytes())
                proof = {name + "-" + role: dict(original=dict(pid=910000 + i * 2 + j, uid=os.geteuid()), esrch=True)
                         for i, name in enumerate(("receipt-receiver", "receipt-sender"))
                         for j, role in enumerate(("owner", "supervisor"))}
                (previous / "artifacts/native-terminal-proof.json").write_text(json.dumps(proof))
                next_plan = retained_setup.replace(plan, case=root / "second-attempt", original_case=previous, original_hashes={})
                next_plan.original_hashes.update({name: hashlib.sha256(next_plan.evidence_path(name).read_bytes()).hexdigest()
                                                 for name in retained_setup.RETAINED_FILES})
                second = retained_setup.RetainedCodexSetup(next_plan, trust_observation=lambda: observed, runner=runner)
                self.assertEqual(second.prepare(), project_id)
            self.assertEqual(second.plan.project, plan.project)
            self.assertEqual(second.plan.state, plan.state)
            self.assertNotEqual(first.plan.case, second.plan.case)
            self.assertEqual(sentinel.read_bytes(), b"original history")
            for name, original in install.items():
                self.assertEqual(next_plan.evidence_path(name).read_bytes(), original)
            self.assertEqual(len(runner.call_args_list), 6)

    def test_startup_retry_refuses_message_history_partial_pages_or_prior_mutations(self):
        for failure in ("message", "partial", "gap", "mutation"):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                plan, observed, _project_id = self.retained(Path(directory).resolve())
                page_path = plan.evidence_path("artifacts/postmortem-event-summary.json")
                page = json.loads(page_path.read_text())
                if failure == "message":page["events"]["items"][1]["kind"] = "message.created"
                if failure == "partial":page["events"]["next_cursor"] = "2"
                if failure == "gap":page["events"]["items"].pop(0)
                if failure == "mutation":
                    commands = plan.original_case / "commands";commands.mkdir()
                    (commands / "unknown-post.json").write_text(json.dumps(dict(kind="post", outcome_unknown=True)))
                page_path.write_text(json.dumps(page))
                plan.original_hashes["artifacts/postmortem-event-summary.json"] = hashlib.sha256(page_path.read_bytes()).hexdigest()
                with mock.patch.object(retained_setup.os, "kill", side_effect=ProcessLookupError), \
                        mock.patch.object(retained_setup, "retained_processes"):
                    with self.assertRaises(cli.CallError):
                        retained_setup.RetainedCodexSetup(plan, trust_observation=lambda: observed, runner=mock.Mock()).prepare()
                self.assertFalse(plan.case.exists())


class PhaseFixture:
    """A fake external result source; never a native or product policy proof."""
    project_id = str(uuid.UUID(int=1))

    def __init__(self):
        self.settings = dict(auto_ack=True, auto_read=True)
        self.row = None
        self.body = None
        self.views = {}
        self.calls = []
        self.delivery = str(uuid.UUID(int=8))
        self.message_id = str(uuid.UUID(int=9))
        self.generation = str(uuid.UUID(int=10))

    def producer(self, name, number):
        identifier = str(uuid.UUID(int=number))
        view = dict(project_id=self.project_id, head="1", actor=dict(actor_id=identifier, kind="agent", handle=name),
                    participation="joined", context_id=identifier, channel_policy=dict(defaults=["native"]),
                    receipt_settings=self.settings, statistics={},
                    native_delivery=dict(generation=self.generation, state="ready", head=None))
        self.views[name] = view
        producer = mock.Mock(name=name)
        producer.name = name
        producer.actor = cli.NativeActor("codex", "opaque-" + name)
        producer.context = dict(context_id=identifier, incarnation=str(uuid.UUID(int=20)))
        producer.selected_whoami.side_effect = lambda _client, _label: copy.deepcopy(view)
        return producer

    def configure(self, _label, *, auto_ack, auto_read):
        self.calls.append((auto_ack, auto_read))
        self.settings.update(auto_ack=auto_ack, auto_read=auto_read)

    def post(self, _label, _actor, _channel, body):
        self.body = body
        read = self.settings["auto_read"]
        ack = self.settings["auto_ack"]
        self.row = dict(recipient=dict(actor_id=self.views["receiver"]["actor"]["actor_id"], kind="agent", handle="receiver"),
                        delivery=dict(delivery_id=self.delivery, state="acknowledged" if ack else "inflight",
                                      accepted_at="accepted-first", acknowledged_at="ack-first" if ack else None,
                                      confirmation=dict(deadline="original-deadline", state="confirmed")),
                        reads=[dict(kind="automatic", at="automatic-first")] if read else [])
        if not ack:
            self.views["receiver"]["native_delivery"].update(state="awaiting_ack", head=dict(
                delivery_id=self.delivery, accepted_at="accepted-first", receipt_policy=copy.deepcopy(self.settings)))
        return dict(result=dict(message_id=self.message_id))

    def message(self, _label, message_id, **_page):
        return dict(result=dict(message=dict(id=message_id, body=self.body), receipts=dict(
            items=[copy.deepcopy(self.row)], next_cursor=None, scan_cursor="1", head="1")))

    def ack(self, _label, _actor, delivery_id):
        assert delivery_id == self.delivery
        self.row["delivery"].update(acknowledged_at="manual-first", state="acknowledged")


class PhaseControllerTests(unittest.TestCase):
    def controller(self, root):
        fixture = PhaseFixture()
        receiver, sender = fixture.producer("receiver", 2), fixture.producer("sender", 3)
        return phase_controller.PhaseController(fixture, [receiver, sender], channel="native", artifacts=root), fixture, receiver, sender

    def test_addressed_body_uses_public_handle_while_artifact_role_stays_fixed(self):
        with tempfile.TemporaryDirectory() as directory:
            controller, fixture, receiver, sender = self.controller(Path(directory))
            fixture.views["receiver"]["actor"]["handle"] = "receipt-receiver-0123456789ab"
            controller.begin("default")
            body = phase_controller.maximum_body("marker", prefix="@receipt-receiver-0123456789ab ")
            self.assertEqual(len(body.encode()), 65_536)
            controller.post(sender, receiver, body, addressed=True)
            self.assertEqual(receiver.name, "receiver")
            with self.assertRaises(frames.AuditError):
                controller.post(sender, receiver, "@receiver old artifact role", addressed=True)
            self.assertEqual(fixture.body, body)

    def test_selector_synchronizes_before_baseline_but_rejects_a_change_inside_it(self):
        for late_change in (False, True):
            with self.subTest(late_change=late_change), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                controller, fixture, receiver, _sender = self.controller(root)
                receiver.host = "codex"
                receiver.proof = dict(owner=dict(pid=12))
                receiver.ready_path = root / "original-ready.json"
                receiver.ready_path.write_text("original physical readiness")
                current = str(uuid.UUID(int=40))
                calls = []

                def selected(_client, label):
                    calls.append(label)
                    descriptor = str(uuid.UUID(int=41)) if late_change and len(calls) >= 3 else current
                    receiver.context = dict(context_id=descriptor)
                    fixture.views["receiver"]["context_id"] = descriptor
                    return copy.deepcopy(fixture.views["receiver"])

                receiver.selected_whoami.side_effect = selected
                campaign = mock.Mock(actors=dict(receiver=receiver), controller=controller)
                campaign.setup.client = fixture
                campaign.label.side_effect = controller.label
                if late_change:
                    with self.assertRaises(frames.AuditError):
                        phase_commands.selector(campaign, "receiver")
                else:
                    result = phase_commands.selector(campaign, "receiver")
                    self.assertEqual(result["context_id"], current)
                    proof = json.loads(next(root.glob("*-selector-proof.json")).read_text())
                    self.assertEqual(proof["before"]["context"], dict(context_id=current))
                    self.assertEqual(proof["before"], proof["after"])
                    self.assertEqual(len(calls), 4)
                    self.assertTrue(calls[0].endswith("selector-synchronize"))
                self.assertFalse(campaign.observers.mock_calls)
                self.assertEqual(fixture.calls, [])

    def test_settled_boundary_and_all_policies_inspect_distinct_external_frames_and_receipts(self):
        for name, ack, read in phase_controller.POLICIES:
            with tempfile.TemporaryDirectory() as directory:
                controller, fixture, receiver, sender = self.controller(Path(directory))
                controller.begin(name)
                body = phase_controller.maximum_body("case-marker", prefix="@receiver ")
                self.assertEqual(len(body.encode()), 65_536)
                message_id = controller.post(sender, receiver, body, addressed=True)
                metadata = frames.decode_frame(message(body, ack, read)).metadata
                metadata.update(project_id=fixture.project_id, message_id=fixture.message_id,
                                delivery_id=fixture.delivery, generation=fixture.generation,
                                author=dict(actor_id=fixture.views["sender"]["actor"]["actor_id"], kind="agent", handle="sender"))
                text = "[AgentChat:Message]\n--\n" + json.dumps(metadata, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n--\n" + body + "\n--\n[/AgentChat:Message]"
                controller.complete_message(message_id, text, generation=fixture.generation)
                with self.assertRaises(frames.AuditError):
                    controller.complete_message(message_id, text, generation=str(uuid.UUID(int=20)))
                if not ack:
                    controller.frozen_policy(receiver, message_id, successor_probe=lambda: False,
                                             original_files=lambda: {"original-text": "same-digest"})
                    controller.manual_ack(receiver, message_id)
                    self.assertEqual(fixture.row["reads"], [dict(kind="automatic", at="automatic-first")] if read else [])
                    with self.assertRaises(frames.AuditError):
                        controller.begin("default")

    def test_changed_original_files_or_premature_successor_cannot_pass_frozen_policy(self):
        with tempfile.TemporaryDirectory() as directory:
            controller, fixture, receiver, sender = self.controller(Path(directory))
            controller.begin("manual-without-read")
            message_id = controller.post(sender, receiver, "body", addressed=False)
            with self.assertRaises(frames.AuditError):
                controller.awaiting_manual_ack(receiver, message_id, successor_probe=lambda: True)
            samples = iter(("original", "changed"))
            with self.assertRaises(frames.AuditError):
                controller.frozen_policy(receiver, message_id, successor_probe=lambda: False,
                                         original_files=lambda: next(samples))

    def test_prompt_requires_fresh_exact_owner_idle_observation(self):
        producer = actor_producer.ActorProducer.__new__(actor_producer.ActorProducer)
        producer.actor = cli.NativeActor("claude", "actual-root")
        producer.context = dict(context_id="actual-descriptor")
        producer.proof = dict(owner=dict(pid=12))
        producer.readiness = lambda: True
        producer.terminal = mock.Mock()
        import time
        observation = dict(root="actual-root", context_id="actual-descriptor", owner=dict(pid=12),
                           phase="idle", pending_requests=[], observed_monotonic_ns=time.monotonic_ns())
        producer.prompt("one-prompt", "normal prompt", idle_observation=observation)
        self.assertEqual(producer.terminal.submit.call_count, 1)
        for change in (dict(root="borrowed"), dict(context_id="stale"), dict(owner=dict(pid=13)),
                       dict(phase="active"), dict(pending_requests=[dict(id=1)]),
                       dict(observed_monotonic_ns=0), dict(observed_monotonic_ns=None),
                       dict(observed_monotonic_ns=time.monotonic_ns() + 20_000_000_000)):
            with self.assertRaises(cli.CallError):
                producer.prompt("must-not-submit", "normal prompt", idle_observation={**observation, **change})
        self.assertEqual(producer.terminal.submit.call_count, 1)

    def test_notice_metadata_does_not_create_message_read_facts(self):
        with tempfile.TemporaryDirectory() as directory:
            controller, fixture, _receiver, _sender = self.controller(Path(directory))
            controller.begin("default")
            frame = controller.complete_notice(event(True, True), generation=str(uuid.UUID(int=1)),
                kind="message.delivery_confirmation_timeout", receipts_before={}, receipts_after={})
            self.assertEqual(frame.kind, "event")
            with self.assertRaises(frames.AuditError):
                controller.complete_notice(event(True, True), generation=str(uuid.UUID(int=1)),
                    kind="message.delivery_confirmation_timeout", receipts_before={},
                    receipts_after=dict(automatic="must-not-appear"))


class OriginalRecoveryTests(unittest.TestCase):
    def original(self, ack=True, read=True):
        identifier = str(uuid.UUID(int=1))
        text = message("actual complete original", ack, read)
        context = dict(project_id=identifier, host="claude", host_session_id=identifier,
                       context_id=identifier, incarnation=identifier)
        marker = dict(context=context, token=identifier, generation=identifier, message_id=identifier,
                      delivery_id=identifier, seq="9007199254740993", attempt=1,
                      receipt_only=False, native_accepted=True, core_completion_held=True, admitted=False)
        frame = recovery_controller.correlate_hold(marker, text, project_id=identifier,
                                                   context=context, generation=identifier)
        capsule = dict(version=3, root=identifier, native_generation=4, context_id=identifier,
                       incarnation=identifier, token=identifier, text=text)
        receipt = {key: capsule[key] for key in ("root", "native_generation", "context_id", "incarnation", "token")}
        receipt.update(version=2, generation=identifier, delivery_id=identifier, seq=marker["seq"],
                       digest=list(bytes.fromhex(frame.sha256)), accepted=True, native_id=str(uuid.UUID(int=7)))
        return marker, frame, capsule, receipt

    def test_positive_original_receipt_requires_complete_digest_token_root_and_native_id(self):
        marker, frame, capsule, receipt = self.original()
        recovery_controller.correlate_claude_original(capsule, receipt, marker, frame)
        for change in (dict(token=str(uuid.UUID(int=2))), dict(root=str(uuid.UUID(int=2))),
                       dict(native_generation=5), dict(context_id=str(uuid.UUID(int=2))),
                       dict(digest=[0] * 32), dict(native_id=None), dict(accepted=False)):
            with self.assertRaises(frames.AuditError):
                recovery_controller.correlate_claude_original(capsule, {**receipt, **change}, marker, frame)
        for change in (dict(native_accepted=False), dict(core_completion_held=False),
                       dict(receipt_only=True), dict(attempt=2), dict(token="guessed")):
            with self.assertRaises(frames.AuditError):
                recovery_controller.correlate_hold({**marker, **change}, capsule["text"],
                    project_id=marker["context"]["project_id"], context=marker["context"], generation=marker["generation"])

    def test_lost_codex_result_keeps_unknown_original_despite_early_manual_ack(self):
        marker, frame, _capsule, _receipt = self.original()
        row = CanonicalReceiptProofTests().row()
        row["delivery"].update(delivery_id=marker["delivery_id"], state="acknowledged",
                               acknowledged_at="manual-before-acceptance", accepted_at=None)
        progress = dict(generation=marker["generation"], state="acceptance_unknown", head=dict(
            delivery_id=marker["delivery_id"], seq=marker["seq"], attempts=1, accepted_at=None))
        recovery_controller.recovered_facts("codex", marker, frame, row, progress, original_native_count=1)
        for altered in ({**progress, "state": "ready"},
                        {**progress, "head": {**progress["head"], "attempts": 2}}):
            with self.assertRaises(frames.AuditError):
                recovery_controller.recovered_facts("codex", marker, frame, row, altered, original_native_count=1)
        changed = copy.deepcopy(row)
        changed["delivery"]["accepted_at"] = "fabricated-from-ACK"
        with self.assertRaises(frames.AuditError):
            recovery_controller.recovered_facts("codex", marker, frame, changed, progress, original_native_count=1)

    def test_claude_original_settlement_respects_each_frozen_policy_without_resubmission(self):
        for ack in (False, True):
            for read in (False, True):
                marker, frame, _capsule, _receipt = self.original(ack, read)
                row = CanonicalReceiptProofTests().row()
                row["delivery"].update(accepted_at="original-accepted", acknowledged_at="original-ACK" if ack else None)
                row["reads"] = [dict(kind="automatic", at="original-automatic")] if read else []
                progress = dict(generation=marker["generation"], state="ready" if ack else "awaiting_ack",
                                head=None if ack else dict(delivery_id=marker["delivery_id"]))
                recovery_controller.recovered_facts("claude", marker, frame, row, progress, original_native_count=1)
                with self.assertRaises(frames.AuditError):
                    recovery_controller.recovered_facts("claude", marker, frame, row, progress, original_native_count=2)

    def test_native_mixed_inputs_reject_duplicates_and_reversed_project_order(self):
        original = message("original complete body")
        later = original.replace('"seq":"9007199254740993"', '"seq":"9007199254740994"')
        first = event(True, True)
        phase_commands.ordered_frames([first, original, later])
        for wrong in ([original, first], [original, original], [later, original]):
            with self.assertRaises(frames.AuditError):
                phase_commands.ordered_frames(wrong)


if __name__ == "__main__":
    unittest.main()

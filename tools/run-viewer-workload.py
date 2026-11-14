#!/usr/bin/env python3
"""Measure the native workload and its actual rendered product viewer separately."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import runpy
import shutil
import signal
import subprocess
import sys
import time
from verified_artifact import verified_cli

ROOT = Path(__file__).resolve().parent.parent
COMMON = runpy.run_path(ROOT / "tools/run-workload.py")


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def build_test(output, target, env, package, name, debug):
    command = ["cargo", "test", "--locked", "--offline",
               "--package", package, "--test", name,
               "--message-format=json", "--target-dir", str(target)]
    command.append("--no-run")
    if not debug:
        command.append("--release")
    print("BUILD", name, flush=True)
    with (output / (name + "-build.log")).open("w") as log:
        result = subprocess.run(command, cwd=ROOT, env=env, stdout=subprocess.PIPE, stderr=log)
        log.write(result.stdout.decode(errors="replace"))
    if result.returncode:
        raise RuntimeError(f"{name} build failed with {result.returncode}; see its build log")
    candidates = []
    for line in result.stdout.splitlines():
        value = json.loads(line)
        if value.get("reason") == "compiler-artifact" and value.get("target", {}).get("name") == name:
            if value.get("executable"):
                candidates.append(Path(value["executable"]))
    if len(candidates) != 1:
        raise RuntimeError(f"Expected one exact {name} executable, got {candidates}")
    destination = output / "images" / name
    shutil.copy2(candidates[0], destination)
    destination.chmod(0o500)
    return destination, {"command": command, "sha256": digest(destination)}


def sample(pid):
    values = COMMON["resources"](pid)
    if values["rss_kib"] > 512 * 1024 or values["fds"] > 256 or values["threads"] > 128:
        raise RuntimeError(f"Owned process {pid} exceeded its resource guard: {values}")
    return values


def viewer_sample(output, parent, elapsed, expected_sha256):
    path = output / "viewer-process.json"
    if not path.exists():
        return None
    try:
        identity = json.loads(path.read_text())
    except json.JSONDecodeError:
        return None  # The retained child may be publishing its terminal record.
    if identity["state"] != "running":
        return None
    if identity["parent_pid"] != parent:
        raise RuntimeError("Viewer is not a child of the exact owned workload")
    if identity["binary"] != str(output / "images/agentchat") or identity["binary_sha256"] != expected_sha256:
        raise RuntimeError("Viewer image does not match the exact frozen build")
    pid = identity["pid"]
    # Check parentage before observing a second-generation child. The workload
    # retains this exact viewer and spawns no replacement in its lifetime.
    process = subprocess.run(["ps", "-p", str(pid), "-o", "ppid="], capture_output=True, text=True)
    if process.returncode or process.stdout.strip() != str(parent):
        return None
    try:
        return {"seconds": elapsed, "pid": pid, **sample(pid)}
    except (FileNotFoundError, ProcessLookupError):
        return None


def stop_group(child):
    if child.poll() is not None:
        return
    os.killpg(child.pid, signal.SIGTERM)
    try:
        child.wait(timeout=5)
    except subprocess.TimeoutExpired:
        os.killpg(child.pid, signal.SIGKILL)
        child.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--target-dir", required=True, type=Path)
    profiles = parser.add_mutually_exclusive_group()
    profiles.add_argument("--smoke", action="store_true")
    profiles.add_argument("--preflight", action="store_true")
    parser.add_argument("--debug", action="store_true", help="Development smoke only")
    args = parser.parse_args()
    if args.debug and not args.smoke:
        parser.error("--debug requires --smoke; acceptance uses optimized binaries")
    target = args.target_dir.resolve()
    if not args.debug and target.is_relative_to(ROOT):
        parser.error("optimized builds require a private target outside the worktree")
    output = args.output.resolve()
    output.mkdir(mode=0o700, parents=True, exist_ok=False)
    (output / "images").mkdir()
    env = dict(os.environ, AGENTCHAT_HOME=str(output / "control-state"),
               AGENTCHAT_USER_CONFIG_ROOT=str(output / "control-config"))
    for key in ["AGENTCHAT_HOME", "AGENTCHAT_USER_CONFIG_ROOT"]:
        Path(env[key]).mkdir(mode=0o700)
    initial = COMMON["source_digest"]()
    workload, workload_build = build_test(output, target, env, "agentchat-runtime", "native_workload", args.debug)
    built_viewer, receipt, prepared = verified_cli()
    expected_profile = "debug" if args.debug else "release"
    if receipt["context"]["profile"] != expected_profile:
        raise RuntimeError("Build the matching Viewer profile through make build first")
    viewer = output / "images/agentchat"
    shutil.copy2(built_viewer, viewer)
    viewer.chmod(0o500)
    viewer_build = {"command": ["make", "build", "PROFILE=" + expected_profile],
                    "sha256": digest(viewer), "build": prepared["build"],
                    "receipt_run": receipt["run"]}
    if COMMON["source_digest"]() != initial:
        raise RuntimeError("Source changed during build; do not start a mismatched workload")
    profile = "smoke" if args.smoke else "preflight" if args.preflight else "ten_minute"
    test = {"smoke": "five_participant_native_workload_smoke",
            "preflight": "one_minute_native_workload_preflight",
            "ten_minute": "ten_minute_native_workload"}[profile]
    command = [str(workload), "--exact", test, "--nocapture"]
    if profile != "smoke":
        command.append("--ignored")
    env.update(AGENTCHAT_WORKLOAD_VIEWER_BINARY=str(viewer), AGENTCHAT_WORKLOAD_OUTPUT=str(output))
    metadata = {"profile": profile, "debug": args.debug, "source_sha256": initial,
                "hardware": COMMON["hardware"](), "workload_build": workload_build,
                "viewer_build": viewer_build, "command": command,
                "runtime_samples": [], "viewer_samples": [],
                "resource_guard_per_process": {"rss_kib": 512 * 1024, "fds": 256, "threads": 128}}
    started = time.monotonic()
    print("START", profile, str(output), flush=True)
    with (output / "test.log").open("w") as log:
        child = subprocess.Popen(command, cwd=ROOT, env=env, stdin=subprocess.DEVNULL,
                                 stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        metadata.update(owned_test_pid=child.pid, state="running")
        try:
            while child.poll() is None:
                elapsed = round(time.monotonic() - started, 3)
                try:
                    metadata["runtime_samples"].append({"seconds": elapsed, **sample(child.pid)})
                except (FileNotFoundError, ProcessLookupError):
                    pass
                observed = viewer_sample(output, child.pid, elapsed, viewer_build["sha256"])
                if observed:
                    metadata["viewer_samples"].append(observed)
                (output / "process.json").write_text(json.dumps(metadata, indent=2) + "\n")
                if len(metadata["runtime_samples"]) % 30 == 0:
                    print("RUNNING", elapsed, len(metadata["viewer_samples"]), flush=True)
                if sys.platform == "darwin" and elapsed > 900:
                    raise RuntimeError("Owned macOS workload exceeded its total deadline")
                time.sleep(1)
        finally:
            stop_group(child)
            metadata.update(state="terminal", exit_code=child.returncode,
                            elapsed_seconds=time.monotonic() - started,
                            final_source_sha256=COMMON["source_digest"]())
            (output / "process.json").write_text(json.dumps(metadata, indent=2) + "\n")
    if child.returncode:
        raise RuntimeError(f"Workload failed with {child.returncode}; see test.log")
    if not metadata["runtime_samples"] or not metadata["viewer_samples"]:
        raise RuntimeError("Both exact owned processes require real resource observations")
    if metadata["final_source_sha256"] != initial:
        raise RuntimeError("Source changed during the run; result is not acceptance evidence")
    if digest(viewer) != viewer_build["sha256"] or digest(workload) != workload_build["sha256"]:
        raise RuntimeError("A frozen executable changed during the run")
    report = json.loads((output / "workload.json").read_text())
    if not report["viewer_http"] or report["viewer_http"]["messages"] != report["messages"]:
        raise RuntimeError("Actual rendered SSE did not audit all retained bodies")
    print("COMPLETE", json.dumps({"output": str(output), "exit_code": 0,
          "rendered_messages": report["viewer_http"]["messages"],
          "timing_enforced": report["timing_enforced"]}), flush=True)


if __name__ == "__main__":
    main()

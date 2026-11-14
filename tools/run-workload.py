#!/usr/bin/env python3
"""Run the optimized synthetic acceptance workload with isolated agentchat roots."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import resource
import runpy
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parent.parent
is_agentchat_file = runpy.run_path(ROOT / "tools/source_scope.py")["is_agentchat_file"]
web_asset_files = runpy.run_path(ROOT / "tools/web_assets.py")["verify"]


def source_digest():
    names = subprocess.check_output(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"],
        cwd=ROOT,
    ).split(b"\0")
    if (ROOT / "crates/agentchat-web/Cargo.toml").is_file():
        names.extend(os.fsencode(name) for name in web_asset_files(ROOT))
    digest = hashlib.sha256()
    for raw in sorted(set(names)):
        if not raw:
            continue
        name = os.fsdecode(raw)
        source_input = (Path(name).suffix in {".rs", ".py", ".toml", ".lock"}
                        or name == "Makefile" or name.startswith(("crates/", "web/", "generated/agentchat-viewer-contract/")))
        if not source_input or not is_agentchat_file(name) or not (ROOT / name).is_file():
            continue
        digest.update(raw + b"\0" + (ROOT / name).read_bytes() + b"\0")
    return digest.hexdigest()


def resources(pid):
    # The caller retains the exact Popen child without reaping during this call;
    # an exit race can disappear, but its PID cannot become another process.
    if sys.platform == "darwin":
        from macos_resources import resources as native_resources
        return native_resources(pid)
    directory = Path("/proc") / str(pid)
    if directory.is_dir():
        values = {}
        for line in (directory / "status").read_text().splitlines():
            key, _, value = line.partition(":")
            if key in {"VmRSS", "VmHWM", "Threads"}:
                values[key] = int(value.split()[0])
        if "VmRSS" not in values or "Threads" not in values:
            raise ProcessLookupError(pid)
        return {"rss_kib": values.get("VmRSS", 0), "peak_rss_kib": values.get("VmHWM", 0),
                "threads": values.get("Threads", 0), "fds": len(list((directory / "fd").iterdir()))}
    raise ProcessLookupError(pid)


def hardware():
    info = {"platform": platform.platform(), "machine": platform.machine(),
            "logical_cpus": os.cpu_count(), "initial_load_average": os.getloadavg()}
    if Path("/proc/cpuinfo").is_file():
        info["cpu"] = next((s.split(":", 1)[1].strip() for s in Path("/proc/cpuinfo").read_text().splitlines()
                            if s.startswith("model name")), "unknown")
        info["memory"] = Path("/proc/meminfo").read_text().splitlines()[0]
        quota = Path("/sys/fs/cgroup/cpu.max")
        info["cpu_quota"] = quota.read_text().strip() if quota.is_file() else None
    else:
        info["cpu"] = subprocess.check_output(["sysctl", "-n", "machdep.cpu.brand_string"], text=True).strip()
        info["memory_bytes"] = int(subprocess.check_output(["sysctl", "-n", "hw.memsize"], text=True))
    return info


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path, help="New owned report directory")
    profiles = parser.add_mutually_exclusive_group()
    profiles.add_argument("--smoke", action="store_true", help="Five-second functional smoke, not T19")
    profiles.add_argument("--preflight", action="store_true",
                          help="One minute at full rates; timing gates on macOS, report-only on Linux")
    args = parser.parse_args()
    profile = "smoke" if args.smoke else "preflight" if args.preflight else "ten_minute"
    output = args.output.resolve()
    output.mkdir(mode=0o700, parents=True, exist_ok=False)
    initial_source = source_digest()
    metadata = {"source_sha256": initial_source, "hardware": hardware(),
                "profile": profile, "samples": [],
                "resource_sampler": "darwin_libproc" if sys.platform == "darwin" else "linux_procfs"}
    command = ["cargo", "test", "--locked", "--release", "--package", "agentchat-runtime",
               "--test", "workload", "--no-run", "--message-format=json"]
    executable = None
    with (output / "build.log").open("w") as log:
        built = subprocess.run(command, cwd=ROOT, stdout=subprocess.PIPE, stderr=log, check=False)
        log.write(built.stdout.decode(errors="replace"))
    if built.returncode:
        raise SystemExit(built.returncode)
    for line in built.stdout.splitlines():
        try:
            artifact = json.loads(line)
        except json.JSONDecodeError:
            continue
        if artifact.get("reason") == "compiler-artifact" and artifact.get("target", {}).get("name") == "workload":
            executable = artifact.get("executable") or executable
    if not executable:
        raise RuntimeError("Cargo did not report the workload test executable")
    env = dict(os.environ, AGENTCHAT_HOME=str(output / "control-state"),
               AGENTCHAT_USER_CONFIG_ROOT=str(output / "control-config"), AGENTCHAT_WORKLOAD_OUTPUT=str(output))
    for key in ["AGENTCHAT_HOME", "AGENTCHAT_USER_CONFIG_ROOT"]:
        Path(env[key]).mkdir(mode=0o700)
    test = {"smoke": "five_participant_workload_smoke",
            "preflight": "one_minute_workload_preflight",
            "ten_minute": "ten_minute_workload"}[profile]
    command = [executable, "--exact", test, "--nocapture"]
    if profile != "smoke":
        command.append("--ignored")
    started = time.monotonic()
    print("START", metadata["profile"], str(output), flush=True)
    with (output / "test.log").open("w") as log:
        before_cpu=resource.getrusage(resource.RUSAGE_CHILDREN)
        child = subprocess.Popen(command, cwd=ROOT, env=env, stdin=subprocess.DEVNULL,
                                 stdout=log, stderr=subprocess.STDOUT)
        metadata["owned_test_pid"] = child.pid
        try:
            while child.poll() is None:
                elapsed = time.monotonic() - started
                try:
                    sample = dict(seconds=round(elapsed, 3), **resources(child.pid))
                    metadata["samples"].append(sample)
                    if sample["rss_kib"] > 512 * 1024 or (sample["fds"] or 0) > 256 or (sample["threads"] or 0) > 128:
                        raise RuntimeError("Workload exceeded the recorded process-resource guard")
                    if len(metadata["samples"]) % 30 == 0:
                        print("RUNNING", json.dumps(sample), flush=True)
                except (FileNotFoundError, ProcessLookupError):
                    pass
                # Keep the existing platform watchdogs; both timing gates are
                # enforced after functional accounting on macOS; Linux reports them.
                if sys.platform == "darwin" and elapsed > 900:
                    raise RuntimeError("Owned macOS workload exceeded its total deadline")
                time.sleep(1)
        finally:
            if child.poll() is None:
                child.terminate()
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
            metadata["exit_code"] = child.returncode
            metadata["elapsed_seconds"] = round(time.monotonic() - started, 3)
            after_cpu=resource.getrusage(resource.RUSAGE_CHILDREN)
            metadata["child_cpu_seconds"] = round(after_cpu.ru_utime+after_cpu.ru_stime-before_cpu.ru_utime-before_cpu.ru_stime,3)
            metadata["final_load_average"] = os.getloadavg()
            metadata["final_source_sha256"] = source_digest()
            (output / "process.json").write_text(json.dumps(metadata, indent=2) + "\n")
    if child.returncode:
        print((output / "test.log").read_text()[-6000:], flush=True)
        raise SystemExit(child.returncode)
    if not metadata["samples"]:
        raise RuntimeError("Workload completed without a process-resource observation")
    if source_digest() != initial_source:
        raise RuntimeError("Source changed during this run; do not use it as acceptance evidence")
    report = json.loads((output / "workload.json").read_text())
    print("COMPLETE", json.dumps({"exit_code": child.returncode,
                                  "timing_enforced": report["timing_enforced"],
                                  "latency_target_passed": report["latency_target_passed"],
                                  "posting_window_passed": report["posting_window_passed"],
                                  "elapsed_seconds": metadata["elapsed_seconds"],
                                  "report": str(output / "workload.json")}), flush=True)


if __name__ == "__main__":
    main()

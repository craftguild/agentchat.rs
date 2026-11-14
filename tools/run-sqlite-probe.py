#!/usr/bin/env python3
"""Compare minimal SeaORM/SQLite operations without the application/runtime."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import plistlib
import resource
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parent.parent
MODES = ["memory", "full", "normal", "full-batch8"]


def source_identity():
    names = ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "crates/agentchat-store/Cargo.toml",
             "crates/agentchat-store/examples/sqlite_probe.rs", "tools/run-sqlite-probe.py"]
    files = {name: hashlib.sha256((ROOT / name).read_bytes()).hexdigest() for name in names}
    return {"files_sha256": files, "combined_sha256": hashlib.sha256(
        json.dumps(files, sort_keys=True).encode()).hexdigest()}


def environment():
    value = {"unix_seconds": time.time(), "load_average": os.getloadavg()}
    if sys.platform == "linux":
        value["aggregate_cpu_ticks"] = [int(x) for x in Path("/proc/stat").read_text().splitlines()[0].split()[1:]]
        value["clock_ticks_per_second"] = os.sysconf("SC_CLK_TCK")
        value["cpu_khz"] = {}
        for path in Path("/sys/devices/system/cpu").glob("cpu[0-9]*/cpufreq/scaling_cur_freq"):
            try:
                value["cpu_khz"][path.parent.parent.name] = int(path.read_text())
            except (OSError, ValueError):
                pass
        value["thermal_millicelsius"] = {}
        for path in Path("/sys/class/thermal").glob("thermal_zone*"):
            try:
                value["thermal_millicelsius"][(path / "type").read_text().strip()] = int((path / "temp").read_text())
            except (OSError, ValueError):
                pass
    return value


def hardware(output):
    value = {"platform": platform.platform(), "machine": platform.machine(), "logical_cpus": os.cpu_count()}
    if sys.platform == "linux":
        value["cpu"] = next(line.split(":", 1)[1].strip() for line in Path("/proc/cpuinfo").read_text().splitlines() if line.startswith("model name"))
        value["memory"] = Path("/proc/meminfo").read_text().splitlines()[0]
        value["boot_id"] = Path("/proc/sys/kernel/random/boot_id").read_text().strip()
        command = ["findmnt", "-J", "-T", str(output), "-o", "FSTYPE,OPTIONS"]
    else:
        value["cpu"] = subprocess.check_output(["sysctl", "-n", "machdep.cpu.brand_string"], text=True).strip()
        value["memory_bytes"] = int(subprocess.check_output(["sysctl", "-n", "hw.memsize"], text=True))
        mount = subprocess.check_output(["df", "-P", str(output)], text=True).splitlines()[-1].split()[-1]
        command = ["diskutil", "info", "-plist", mount]
    result = subprocess.run(command, capture_output=True, text=True, check=False)
    filesystem = result.stdout.strip()
    if sys.platform == "darwin" and result.returncode == 0:
        fields = plistlib.loads(result.stdout.encode())
        filesystem = {key: fields[key] for key in ["FilesystemType", "FilesystemName", "MountPoint", "SolidState", "DeviceBlockSize"] if key in fields}
    value["filesystem"] = {"command": command, "exit_code": result.returncode, "output": filesystem}
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path, help="New owned result directory on the tested filesystem")
    parser.add_argument("--rows", type=int, default=2048)
    parser.add_argument("--repeat", type=int, default=3)
    args = parser.parse_args()
    if args.rows < 8 or args.rows > 1_000_000 or args.rows % 8 or not 1 <= args.repeat <= 20:
        parser.error("rows must be 8..1000000 and divisible by eight; repeat must be 1..20")
    os.umask(0o077)
    output = args.output.resolve()
    output.mkdir(mode=0o700, parents=True, exist_ok=False)
    identity = source_identity()
    metadata = {"version": 1, "started_unix": time.time(), "source": identity,
                "hardware": hardware(output), "rows": args.rows, "repeat": args.repeat,
                "build_command": ["cargo", "build", "--locked", "--release", "-p", "agentchat-store", "--example", "sqlite_probe", "--message-format=json"],
                "runs": [], "scope": "isolated sequential diagnostic; not application/host/crash acceptance"}
    env = dict(os.environ)
    env.pop("AGENTCHAT_CONTEXT", None)
    for key, name in [("AGENTCHAT_HOME", "control-state"), ("AGENTCHAT_USER_CONFIG_ROOT", "control-config")]:
        path = output / name
        path.mkdir(mode=0o700)
        env[key] = str(path)
    build = subprocess.run(metadata["build_command"], cwd=ROOT, env=env, capture_output=True, check=False)
    (output / "build.stdout.jsonl").write_bytes(build.stdout)
    (output / "build.stderr.log").write_bytes(build.stderr)
    if build.returncode:
        raise SystemExit(build.returncode)
    artifacts = [json.loads(line) for line in build.stdout.splitlines() if line.startswith(b"{")]
    executable = next(item["executable"] for item in reversed(artifacts)
                      if item.get("reason") == "compiler-artifact" and item.get("target", {}).get("name") == "sqlite_probe" and item.get("executable"))
    executable_hash = hashlib.sha256(Path(executable).read_bytes()).hexdigest()
    metadata.update(executable=executable, executable_sha256=executable_hash)
    report_path = output / "suite.json"
    for repetition in range(args.repeat):
        # Rotate mode order across repetitions to reduce a fixed-order bias.
        offset = repetition % len(MODES)
        for mode in MODES[offset:] + MODES[:offset]:
            assert source_identity() == identity
            assert hashlib.sha256(Path(executable).read_bytes()).hexdigest() == executable_hash
            label = f"{repetition + 1:02}-{mode}"
            command = [executable, mode, str(args.rows), str(output / label)]
            before_cpu = resource.getrusage(resource.RUSAGE_CHILDREN)
            before = environment()
            started = time.monotonic()
            # run() retains and reaps this exact child; a timeout kills only it.
            child = subprocess.run(command, cwd=ROOT, env=env, stdin=subprocess.DEVNULL,
                                   capture_output=True, timeout=300, check=False)
            wall = time.monotonic() - started
            after = environment()
            after_cpu = resource.getrusage(resource.RUSAGE_CHILDREN)
            (output / (label + ".stdout.log")).write_bytes(child.stdout)
            (output / (label + ".stderr.log")).write_bytes(child.stderr)
            run = {"label": label, "mode": mode, "repetition": repetition + 1, "command": command,
                   "exit_code": child.returncode, "wall_seconds": wall,
                   "user_cpu_seconds": after_cpu.ru_utime - before_cpu.ru_utime,
                   "system_cpu_seconds": after_cpu.ru_stime - before_cpu.ru_stime,
                   "environment_before": before, "environment_after": after}
            if child.returncode == 0:
                run["result"] = json.loads(child.stdout)
            metadata["runs"].append(run)
            report_path.write_text(json.dumps(metadata, indent=2) + "\n")
            print(json.dumps({"label": label, "exit_code": child.returncode,
                              "wall_seconds": round(wall, 3), "write_ms": run.get("result", {}).get("write_ms")}), flush=True)
            if child.returncode:
                raise SystemExit(child.returncode)
    assert source_identity() == identity
    metadata.update(finished_unix=time.time(), final_source=source_identity())
    report_path.write_text(json.dumps(metadata, indent=2) + "\n")
    print("COMPLETE", str(report_path), flush=True)


if __name__ == "__main__":
    main()

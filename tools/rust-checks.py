"""Invoke the product's existing Rust checks in the selected prepared profile."""
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parent.parent
profile = os.environ.get("CRAFTGUILD_PROFILE", os.environ.get("PROFILE", "release"))
if profile not in {"debug", "release"}:
    raise SystemExit("Expected debug or release profile")
test_target = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")) / "product-tests"
flags = ["--target-dir", str(test_target)]
if profile == "release":
    flags.append("--release")
if target := os.environ.get("CRAFTGUILD_TARGET"):
    flags.extend(["--target", target])
mode = sys.argv[1]
commands = {
    "format": ["cargo", "fmt", "--all", "--", "--check"],
    "lint": ["cargo", "clippy", "--locked", "--workspace", "--all-targets", *flags, "--", "-D", "warnings"],
    "test": [sys.executable, "tools/test-isolated.py", "cargo", "test", "--locked", "--workspace", "--no-fail-fast", *flags],
}
raise SystemExit(subprocess.run(commands[mode], cwd=ROOT).returncode)

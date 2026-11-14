"""Orchestrate Rust artifact acceptance and common copy-only staged installation."""

import os
from pathlib import Path
import subprocess
import tempfile
from verified_artifact import verified_cli

ROOT = Path(__file__).resolve().parent.parent
entry = ["python3", "standards/launcher.py"]
binary, receipt, prepared = verified_cli()
context = receipt["context"]
environment = dict(os.environ, AGENTCHAT_STANDALONE_BINARY=str(binary),
                   AGENTCHAT_STANDALONE_SBOM=str(ROOT / prepared["sbom"]),
                   AGENTCHAT_STANDALONE_REVISION=prepared["build"]["commit"])
flags = ["--target", context["target"], "--target-dir",
         str(Path(context["target_dir"]) / "product-tests")]
if context["profile"] == "release":
    flags.append("--release")
subprocess.run(["python3", "tools/test-isolated.py", "cargo", "test", "--locked",
                "-p", "agentchat-cli", "--test", "standalone", *flags, "--",
                "--ignored", "--nocapture"], cwd=ROOT, env=environment, check=True)
with tempfile.TemporaryDirectory(prefix="agentchat-staged-install-") as stage:
    subprocess.run([*entry, "install"], cwd=ROOT, check=True,
                   env=dict(os.environ, DESTDIR=stage, PREFIX="/usr/local"))
print("PASS: actual standalone CLI and common verified staged installation")

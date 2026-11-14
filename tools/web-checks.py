"""Select existing Rust providers and run the product's actual API/browser suites."""

import json
import os
from pathlib import Path
import subprocess
from verified_artifact import verified_cli

ROOT = Path(__file__).resolve().parent.parent
binary, receipt, _ = verified_cli()
profile = receipt["context"]["profile"]
flags = ["--target", receipt["context"]["target"], "--target-dir",
         str(Path(receipt["context"]["target_dir"]) / "product-tests")]
if profile == "release":
    flags.append("--release")
environment = dict(os.environ, AGENTCHAT_REAL_BROWSER_CLI_BINARY=str(binary))
for package, target, variable in [
    ("agentchat-cli", "real_browser_provider", "AGENTCHAT_REAL_BROWSER_BINARY"),
    ("agentchat-web", "browser_provider", "AGENTCHAT_WEB_BROWSER_BINARY"),
]:
    command = ["cargo", "test", "--locked", "--offline", "--no-run", "--message-format=json",
               "-p", package, "--test", target, *flags]
    output = subprocess.check_output(command, cwd=ROOT, env=environment, text=True)
    candidates = [value["executable"] for line in output.splitlines()
                  if (value := json.loads(line)).get("reason") == "compiler-artifact"
                  and value.get("target", {}).get("name") == target and value.get("executable")]
    if len(candidates) != 1:
        raise SystemExit("Expected exactly one built provider: " + target)
    environment[variable] = candidates[0]
verified_cli()  # Provider compilation must preserve the sealed distribution.
for name in ["test:integration", "test:browser"]:
    subprocess.run(["npm", "run", name, "--prefix", "web"], cwd=ROOT,
                   env=environment, check=True)

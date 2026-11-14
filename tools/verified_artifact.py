"""Read the selected product CLI only after common receipt verification."""

import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parent.parent


def verified_cli():
    subprocess.run(["python3", "standards/launcher.py", "verify-receipt", "--artifact", "agentchat"],
                   cwd=ROOT, check=True)
    receipt = json.loads((ROOT / ".craftguild/agentchat/receipt.json").read_text())
    prepared = json.loads((ROOT / ".craftguild/agentchat/prepared.json").read_text())
    return Path(receipt["context"]["output"]), receipt, prepared

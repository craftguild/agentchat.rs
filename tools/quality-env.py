"""Expose this product's prepared npm parser and published Mock tool to checks."""

import json
from pathlib import Path
from chigiri_contract import generator

root = Path(__file__).resolve().parent.parent
parser = root / "web/node_modules/typescript/package.json"
if not parser.is_file():
    raise SystemExit("Prepare locked frontend dependencies with npm ci --prefix web --ignore-scripts")
environment = {"NODE_PATH": str(root / "web/node_modules"),
               "CHIGIRI_BIN": str(generator(root))}
path = root / ".craftguild/quality-environment.json"
path.parent.mkdir(exist_ok=True)
path.write_text(json.dumps(environment, indent=2) + "\n")

#!/usr/bin/env python3
"""Add fresh-fixture lifecycle audit hooks without changing other settings."""

import argparse
import json
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("settings", type=Path)
    parser.add_argument("hook", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    if not all(path.is_absolute() for path in (args.settings, args.hook, args.output)):
        parser.error("all paths must be absolute")
    data = json.loads(args.settings.read_text())
    command = f"python3 {args.hook} {args.output}"
    hooks = data.setdefault("hooks", {})
    for name in ("SessionStart", "SessionEnd"):
        hooks.setdefault(name, []).append(
            {
                "hooks": [
                    {"type": "command", "command": command, "timeout": 5}
                ]
            }
        )
    args.settings.write_text(json.dumps(data, indent=2) + "\n")


if __name__ == "__main__":
    main()

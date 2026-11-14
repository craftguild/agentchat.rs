#!/usr/bin/env python3
"""Run a command with disposable AgentChat state/config roots, preserving host homes."""

import os
import pathlib
import subprocess
import sys
import tempfile


def main():
    if len(sys.argv) < 2:
        sys.exit("usage: python3 tools/test-isolated.py COMMAND [ARG...]")
    with tempfile.TemporaryDirectory(prefix="agentchat-test-") as directory:
        root = pathlib.Path(directory)
        env = os.environ.copy()
        for variable, name in [("AGENTCHAT_HOME", "state"), ("AGENTCHAT_USER_CONFIG_ROOT", "config")]:
            path = root / name
            path.mkdir(mode=0o700)
            env[variable] = str(path)
        # HOME and CODEX_HOME are inherited verbatim. Tests must never redirect
        # either variable to impersonate a different host environment.
        result = subprocess.run(sys.argv[1:], env=env, check=False)
        sys.exit(result.returncode)


if __name__ == "__main__":
    main()

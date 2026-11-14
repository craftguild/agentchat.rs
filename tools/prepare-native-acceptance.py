#!/usr/bin/env python3
"""Prepare and verify a disposable actual-native fixture without starting models.

Uses the repository's native_acceptance Cargo example and real agentchat executable.
Existing host configuration and credentials are never selected or copied. This
checks installation/preparation only; it does not prove live API compatibility
or complete real-host acceptance.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def fingerprint(path):
    """Identify the exact executable used by the retained preparation report."""
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def prepare(args):
    """Create one fresh fixture and retain failed outputs as well as successes."""
    if args.output is None:
        root = Path(tempfile.mkdtemp(prefix="agentchat-native-acceptance-")).resolve()
    else:
        root = args.output.absolute()
        root.mkdir(mode=0o700)
        root = root.resolve()
    print(json.dumps({"fixture": str(root)}), flush=True)
    config = root / "config"
    profile = config / "host-config"
    profile.mkdir(parents=True, mode=0o700)
    for name in ("bin", "project", "init-bin"):
        (root / name).mkdir(mode=0o700)
    # Fresh destination inodes avoid replacing an executable used by another
    # retained case, including macOS executable-vnode cache interactions.
    for source, name in ((args.agentchat, "agentchat"), (args.helper, "probe")):
        target = root / "bin" / name
        shutil.copyfile(source, target)
        target.chmod(0o700)
    native = args.native.resolve(strict=True)
    foreign = {"command": "/nonexistent/agentchat-acceptance-foreign-sentinel"}
    registry = profile / ".claude.json"
    registry.write_text(json.dumps({"mcpServers": {"fixture-foreign": foreign}}) + "\n")
    sentinel = {"matcher": ".foreign-probe", "hooks": [
        {"type": "command", "command": "/usr/bin/true"}]}
    settings = profile / "settings.json"
    settings.write_text(json.dumps({"hooks": {"FileChanged": [sentinel]},
                                   "env": {"AGENTCHAT_FIXTURE_SENTINEL": "preserved"}}) + "\n")
    env = os.environ.copy()
    env.pop("AGENTCHAT_CONTEXT", None)
    env.update(AGENTCHAT_HOME=str(root / "state"), AGENTCHAT_USER_CONFIG_ROOT=str(config),
               CLAUDE_CONFIG_DIR=str(profile), PATH=str(root / "init-bin"))
    # HOME and CODEX_HOME stay inherited. Every selected installation target
    # and native configuration path is under this newly created fixture.
    def run(name, argv, structured=True):
        result = subprocess.run([str(v) for v in argv], env=env,
                                stdin=subprocess.DEVNULL, capture_output=True,
                                text=True, timeout=30, check=False)
        (root / (name + ".stdout")).write_text(result.stdout)
        (root / (name + ".stderr")).write_text(result.stderr)
        if result.returncode:
            raise RuntimeError(f"{name} failed ({result.returncode}); see {root}")
        return json.loads(result.stdout) if structured else result.stdout

    version = run("native-version", [native, "--version"], structured=False)
    if version.split()[0] != args.version:
        raise RuntimeError(f"The native candidate requires exactly Claude {args.version}")
    probe = root / "bin/probe"
    plan = run("plan", [probe, "plan", root, config, native, "--version", args.version])
    assert not (root / "state").exists(), "Planning wrote state"
    assert json.loads(registry.read_text()) == {"mcpServers": {"fixture-foreign": foreign}}
    installed = run("install", [probe, "install", root, config, native, "--version", args.version])
    assert installed["repeated"]["changed_files"] == 0
    assert installed["health"]["state"] == "healthy"
    command = [root / "bin/agentchat", "--json", "--project", root / "project",
               "install", "--scope", "directory"]
    first = run("public-init", command)
    repeated = run("public-init-repeat", command)
    assert first["project_id"] == repeated["project_id"]
    assert first["result"]["created"] and not repeated["result"]["created"]
    capabilities = repeated["result"]["capabilities"]
    assert capabilities["compatibility"] == "best_effort_api_surface"
    assert capabilities["version_policy"] == "diagnostic_and_identity_only"
    servers = json.loads(registry.read_text())["mcpServers"]
    assert servers["fixture-foreign"] == foreign and servers["agentchat"] == plan["mcp"]
    current = json.loads(settings.read_text())
    assert current["env"] == {"AGENTCHAT_FIXTURE_SENTINEL": "preserved"}
    assert sentinel in current["hooks"]["FileChanged"]
    validation = run("native-validate", [native, "plugin", "validate",
                                         profile / "skills/agentchat", "--json"])
    assert validation["success"]
    report = {"passed": True, "fixture": str(root), "native_version": args.version,
              "plan_read_only": True, "repeated_install_unchanged": True,
              "public_init_idempotent": True, "native_plan_preserved": True,
              "foreign_values_preserved": True, "native_plugin_valid": True,
              "binaries": {name: fingerprint(root / "bin" / name)
                           for name in ("agentchat", "probe")},
              "models_started": 0, "compatibility": capabilities["compatibility"],
              "real_host_acceptance": "not_attempted"}
    (root / "summary.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--agentchat", type=Path, required=True)
    parser.add_argument("--helper", type=Path, required=True)
    parser.add_argument("--native", type=Path, required=True)
    parser.add_argument("--version", required=True,
                        help="Observed native version for this fixture; not a compatibility allowlist")
    parser.add_argument("--output", type=Path, help="Fresh, nonexistent fixture directory")
    args = parser.parse_args()
    for path in (args.agentchat, args.helper, args.native):
        if not path.is_file():
            parser.error(f"Executable is unavailable: {path}")
    prepare(args)


if __name__ == "__main__":
    main()

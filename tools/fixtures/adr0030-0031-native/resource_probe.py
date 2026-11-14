"""Read-only fixture resource observation; never admits or changes a profile.

Process command/environment text is used in memory for exact profile matching
and is never emitted. Only selected configuration hashes and file metadata are
retained; authentication contents are neither read nor copied.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import time


def file_fact(path, *, digest=True):
    path = Path(path)
    try:
        value = path.lstat()
    except FileNotFoundError:
        return {"path": str(path), "exists": False}
    result = dict(path=str(path), exists=True, uid=value.st_uid, device=value.st_dev,
                  inode=value.st_ino, mode=stat.S_IMODE(value.st_mode),
                  size=value.st_size, mtime_ns=value.st_mtime_ns,
                  symlink=stat.S_ISLNK(value.st_mode), regular=stat.S_ISREG(value.st_mode))
    if digest and result["regular"] and not result["symlink"]:
        result["sha256"] = hashlib.sha256(path.read_bytes()).hexdigest()
    return result


def profile_processes(profile, *, runner=subprocess.run):
    """A bounded observation, not a native owner or root authorization.

    Enumerate the caller's process environments without exposing them. A failed
    enumeration is unknown rather than a successful quiescence assertion. The
    profile is checked again immediately before any separately authorized write.
    """
    if not profile:
        return dict(state="not_applicable", pids=[])
    result = runner(["ps", "eww", "-U", str(os.geteuid()), "-o", "pid=,command="],
                    capture_output=True, text=True, timeout=15, check=False)
    if result.returncode:
        return dict(state="unknown", pids=[], reason="process enumeration failed")
    needle = "CLAUDE_CONFIG_DIR=" + str(Path(profile).resolve())
    pids = []
    for line in result.stdout.splitlines():
        fields = line.strip().split(None, 1)
        if len(fields) == 2 and needle in fields[1]:
            # A conservative substring match can stop preparation; it can never
            # authorize reuse of a process, an identity or a profile.
            pids.append(int(fields[0]))
    return dict(state="busy" if pids else "observed_quiet", pids=sorted(set(pids)),
                scope="caller-uid ps eww snapshot; recheck before scoped preparation")


def probe(*, host, case, profile=None, native=None):
    case = Path(case)
    if not case.is_absolute():
        raise ValueError("An explicit absolute future case path is required")
    selected = [Path.home() / ".codex/config.toml", Path.home() / ".codex/hooks.json",
                Path.home() / ".claude/settings.json", Path.home() / ".claude.json"]
    if profile:
        selected += [Path(profile) / "settings.json", Path(profile) / ".claude.json"]
    return dict(version=1, platform=sys.platform, host=host, observed_unix_ns=time.time_ns(),
                uid=os.geteuid(), case=file_fact(case, digest=False),
                future_paths={name: str(case / name) for name in
                              ("project", "state", "config", "images", "artifacts")},
                native=None if not native else file_fact(Path(native).resolve()),
                profile=None if not profile else file_fact(Path(profile), digest=False),
                profile_processes=profile_processes(profile),
                configuration=[file_fact(path) for path in selected],
                authentication_metadata=[] if not profile else [
                    file_fact(Path(profile) / ".credentials.json", digest=False)],
                home_inherited=os.environ.get("HOME"),
                codex_home_inherited=os.environ.get("CODEX_HOME"),
                native_executed=False, profile_modified=False)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", choices=("codex", "claude"), required=True)
    parser.add_argument("--case", required=True)
    parser.add_argument("--profile")
    parser.add_argument("--native")
    options = vars(parser.parse_args())
    print(json.dumps(probe(**options), indent=2))

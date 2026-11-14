"""Exact selected-configuration preimages for a separately approved test scope.

Real home files are hash-only resource observations. This module can snapshot
only the two explicit Claude fixture configuration files, never authentication.
Restoration requires original owners positively gone and a fresh quiet check;
unexpected current contents or filesystem identity withhold the write.
"""

import hashlib
import json
import os
from pathlib import Path
import stat
import uuid

from cli_adapter import CallError, save_record
from resource_probe import profile_processes


def secret_free(value):
    if isinstance(value, dict):
        for key, child in value.items():
            normalized = key.lower().replace("_", "").replace("-", "")
            if isinstance(child, str) and any(word in normalized for word in (
                    "token", "apikey", "password", "secret", "authorization", "credential")):
                return False
            if not secret_free(child):
                return False
    if isinstance(value, list):
        return all(secret_free(child) for child in value)
    return True


def selected_bytes(path):
    before = path.lstat()
    if not stat.S_ISREG(before.st_mode) or before.st_uid != os.geteuid() or before.st_size > 16 * 1024 * 1024:
        raise CallError("Unexpected selected configuration ownership/type/size")
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        opened = os.fstat(descriptor)
        if (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino):
            raise CallError("Selected configuration changed during observation")
        with os.fdopen(descriptor, "rb", closefd=False) as source:
            raw = source.read()
    finally:
        os.close(descriptor)
    after = path.lstat()
    if (after.st_dev, after.st_ino, after.st_mtime_ns, after.st_size) != (
            before.st_dev, before.st_ino, before.st_mtime_ns, before.st_size):
        raise CallError("Selected configuration changed during observation")
    return raw, before


class SelectedConfiguration:
    def __init__(self, profile, artifacts):
        self.profile = Path(profile)
        self.artifacts = Path(artifacts)
        self.manifest = {}

    def capture(self):
        if self.manifest or profile_processes(self.profile)["state"] != "observed_quiet":
            raise CallError("Fixture configuration capture requires a fresh quiet profile")
        pending = {}
        for name in ("settings.json", ".claude.json"):
            path = self.profile / name
            if not path.exists() and not path.is_symlink():
                pending[name] = None
                continue
            raw, metadata = selected_bytes(path)
            if not secret_free(json.loads(raw)):
                raise CallError("Configuration contains authentication-like content; no copy made")
            pending[name] = raw, metadata
        # Validate all files before retaining any preimage.
        self.artifacts.mkdir(parents=True, mode=0o700, exist_ok=False)
        for name, value in pending.items():
            if value is None:
                self.manifest[name] = dict(exists=False)
                continue
            raw, metadata = value
            with (self.artifacts / name).open("xb") as output:
                os.chmod(output.name, 0o600)
                output.write(raw)
                output.flush()
                os.fsync(output.fileno())
            self.manifest[name] = dict(exists=True, sha256=hashlib.sha256(raw).hexdigest(),
                                       mode=stat.S_IMODE(metadata.st_mode),
                                       device=metadata.st_dev, inode=metadata.st_ino)
        save_record(self.artifacts / "manifest.json", self.manifest, exclusive=True)
        return self.manifest

    def restore(self, *, owners_gone, expected_current):
        if not self.manifest or owners_gone is not True:
            raise CallError("Positive loss of every original test owner is required")
        if profile_processes(self.profile)["state"] != "observed_quiet":
            raise CallError("Profile quiescence is unknown; restoration withheld")
        for name, original in self.manifest.items():
            path = self.profile / name
            current = selected_bytes(path) if path.exists() or path.is_symlink() else None
            digest = None if current is None else hashlib.sha256(current[0]).hexdigest()
            if digest != expected_current[name]:
                raise CallError("Selected profile changed after the terminal observation")
            if original["exists"]:
                raw = (self.artifacts / name).read_bytes()
                if hashlib.sha256(raw).hexdigest() != original["sha256"]:
                    raise CallError("Original configuration preimage changed")
                stage = path.with_name(".matrix-restore-" + str(uuid.uuid4()))
                with stage.open("xb") as output:
                    os.chmod(stage, original["mode"])
                    output.write(raw)
                    output.flush()
                    os.fsync(output.fileno())
                if current is not None:
                    now, before = path.lstat(), current[1]
                    keys = ("st_dev", "st_ino", "st_uid", "st_mode", "st_mtime_ns", "st_size")
                    if any(getattr(now, key) != getattr(before, key) for key in keys):
                        raise CallError("Selected profile changed immediately before restoration")
                elif path.exists() or path.is_symlink():
                    raise CallError("Selected profile appeared immediately before restoration")
                stage.replace(path)
            elif current is not None:
                # Retain a newly produced selected config instead of deleting it.
                path.rename(self.artifacts / (name + ".produced"))
            directory = os.open(path.parent, os.O_RDONLY)
            try:
                os.fsync(directory)
            finally:
                os.close(directory)

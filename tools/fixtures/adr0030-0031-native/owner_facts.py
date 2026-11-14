"""Independent kernel birth/UID observation for explicitly owned test processes.

This preserves the historical native-rig observation mechanism, with executable
paths deliberately absent from ongoing lifetime equality (ADR 0027). A live PID
or working directory alone never supplies a native conversation identity.
"""

import ctypes
import os
from pathlib import Path
import subprocess
import sys
import uuid


if sys.platform == "darwin":
    class BSDInfo(ctypes.Structure):
        _fields_ = ([(name, ctypes.c_uint32) for name in
                     "flags status xstatus pid ppid uid gid ruid rgid svuid svgid rfu".split()]
                    + [("comm", ctypes.c_char * 16), ("name", ctypes.c_char * 32)]
                    + [(name, ctypes.c_uint32) for name in
                       "nfiles pgid pjobc tdev tpgid nice".split()]
                    + [("seconds", ctypes.c_uint64), ("microseconds", ctypes.c_uint64)])

    library = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)


def boot():
    if sys.platform == "darwin":
        raw = subprocess.check_output(["/usr/sbin/sysctl", "-n", "kern.bootsessionuuid"], text=True)
    else:
        raw = Path("/proc/sys/kernel/random/boot_id").read_text()
    return str(uuid.UUID(raw.strip()))


def sample(pid):
    if sys.platform == "darwin":
        value = BSDInfo()
        count = library.proc_pidinfo(pid, 3, 0, ctypes.byref(value), ctypes.sizeof(value))
        if count != ctypes.sizeof(value) or value.pid != pid or value.status not in (2, 3, 4):
            raise RuntimeError("Kernel birth is unavailable or the owned process is not live")
        if (not value.seconds or value.microseconds >= 1_000_000
                or not value.uid == value.ruid == value.svuid == os.geteuid()):
            raise RuntimeError("Unsupported owned-process birth or credentials")
        return dict(platform="macos", seconds=value.seconds, microseconds=value.microseconds), value.uid
    raw = Path(f"/proc/{pid}/stat").read_text()
    prefix, rest = raw.rsplit(")", 1)
    fields = rest.split()
    if int(prefix.split(" (", 1)[0]) != pid or fields[0] not in "RSDTtKWPI" or int(fields[19]) <= 0:
        raise RuntimeError("Kernel birth is unavailable or the owned process is not live")
    lines = [line for line in Path(f"/proc/{pid}/status").read_text().splitlines() if line.startswith("Uid:")]
    if len(lines) != 1:
        raise RuntimeError("No exact process UID observation")
    values = list(map(int, lines[0][4:].split()))
    if len(values) != 4 or any(value != os.geteuid() for value in values):
        raise RuntimeError("Owned-process credentials changed")
    return dict(platform="linux", ticks=int(fields[19])), values[0]


def facts(pid):
    before = boot()
    first = sample(pid)
    if first != sample(pid) or boot() != before:
        raise RuntimeError("Kernel process continuity is uncertain")
    return dict(pid=pid, boot=before, start=first[0], uid=first[1])


def matches(token):
    return facts(token["pid"]) == {key: token[key] for key in ("pid", "boot", "start", "uid")}


def stopped(token):
    if not matches(token):
        return False
    if sys.platform == "darwin":
        value = BSDInfo()
        count = library.proc_pidinfo(token["pid"], 3, 0, ctypes.byref(value), ctypes.sizeof(value))
        return count == ctypes.sizeof(value) and value.status == 4 and matches(token)
    raw = Path(f'/proc/{token["pid"]}/stat').read_text()
    state = raw.rsplit(")", 1)[1].split()[0]
    return state in ("T", "t") and matches(token)

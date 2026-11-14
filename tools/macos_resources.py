"""Bounded native resource samples for an owned, unreaped macOS child process."""

import ctypes
import errno
from functools import lru_cache
import sys


class _TaskInfo(ctypes.Structure):
    # SDK sys/proc_info.h: PROC_PIDTASKINFO, six uint64 and twelve int32 fields.
    _fields_ = [(name, ctypes.c_uint64) for name in (
        "virtual resident total_user total_system threads_user threads_system".split())] + [
        (name, ctypes.c_int32) for name in (
            "policy faults pageins cow_faults messages_sent messages_received "
            "syscalls_mach syscalls_unix switches threads running priority".split())]


@lru_cache(maxsize=1)
def _library():
    if sys.platform != "darwin":
        raise RuntimeError("Native macOS resource sampling requires Darwin")
    if ctypes.sizeof(_TaskInfo) != 96:
        raise RuntimeError("Unexpected macOS task-info ABI size")
    library = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    library.proc_pidinfo.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_uint64,
                                    ctypes.c_void_p, ctypes.c_int]
    library.proc_pidinfo.restype = ctypes.c_int
    return library


def _query(pid, flavor, buffer, size):
    ctypes.set_errno(0)
    result = _library().proc_pidinfo(pid, flavor, 0, buffer, size)
    if result <= 0:
        error = ctypes.get_errno()
        if error in (0, errno.ESRCH, errno.ENOENT):
            raise ProcessLookupError(pid)
        raise OSError(error, "Native resource observation failed")
    return result


def resources(pid):
    """Return RSS KiB, live threads and exact observed FD count for an owned child.

    The caller must retain its Popen child and must not reap it until observation
    finishes. An exited child retains its PID while unreaped, preventing reuse
    during the sample. This is process measurement, never conversation identity.
    Missing, truncated or inaccessible observations fail instead of becoming zero.
    """
    if pid <= 0:
        raise ValueError("Expected a positive owned child PID")
    task = _TaskInfo()
    filled = _query(pid, 4, ctypes.byref(task), ctypes.sizeof(task))
    if filled != ctypes.sizeof(task):
        raise RuntimeError("Incomplete native task observation")
    # PROC_PIDLISTFDS (1) returns bytes of 8-byte proc_fdinfo entries. Its initial
    # size query includes spare capacity; only the filled second result counts.
    estimate = _query(pid, 1, None, 0)
    capacity = estimate + 256
    if capacity > 65536:
        raise RuntimeError("Native descriptor observation exceeds its buffer budget")
    buffer = ctypes.create_string_buffer(capacity)
    filled = _query(pid, 1, buffer, capacity)
    if filled >= capacity or filled % 8:
        raise RuntimeError("Native descriptor observation is truncated or misaligned")
    return {"rss_kib": task.resident // 1024, "threads": task.threads, "fds": filled // 8}

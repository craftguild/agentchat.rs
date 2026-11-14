#!/usr/bin/env python3
"""Append only selected Claude lifecycle metadata from a command hook."""

import json
import os
import sys
import time


def main():
    if len(sys.argv) != 2 or not os.path.isabs(sys.argv[1]):
        raise SystemExit("an absolute output path is required")
    event = json.load(sys.stdin)
    selected = {
        "observed_unix_ns": time.time_ns(),
        "hook_event_name": event.get("hook_event_name"),
        "session_id": event.get("session_id"),
        "source": event.get("source"),
        "reason": event.get("reason"),
    }
    payload = (json.dumps(selected, sort_keys=True) + "\n").encode()
    descriptor = os.open(
        sys.argv[1], os.O_APPEND | os.O_CREAT | os.O_WRONLY, 0o600
    )
    try:
        os.write(descriptor, payload)
    finally:
        os.close(descriptor)


if __name__ == "__main__":
    main()

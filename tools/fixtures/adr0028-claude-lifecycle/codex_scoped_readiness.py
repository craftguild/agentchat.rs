#!/usr/bin/env python3
"""Probe exact-root Codex metadata and MCP readiness without model input.

The probe opens one connection to an already-running, explicitly selected
app-server daemon. It allocates two fresh roots on that connection, reads only
their metadata and MCP catalog, and prints a redacted result. It never sends a
turn, invokes an MCP tool, joins AgentChat, or stops the daemon.
"""

import argparse
import base64
import hashlib
import json
import os
import socket
import struct
import time


class RpcError(RuntimeError):
    """Native JSON-RPC error with its exact code retained."""

    def __init__(self, value):
        super().__init__(json.dumps(value, sort_keys=True))
        self.value = value


class Codex:
    """Minimal WebSocket JSON-RPC client for the local daemon control socket."""

    limit = 16 * 1024 * 1024

    def __init__(self, path):
        self.socket = socket.socket(socket.AF_UNIX)
        self.socket.settimeout(3)
        self.buffer = b""
        self.counter = 0
        try:
            self.socket.connect(path)
            key = base64.b64encode(os.urandom(16)).decode()
            self.socket.sendall(
                (
                    "GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n"
                    "Connection: Upgrade\r\nSec-WebSocket-Version: 13\r\n"
                    f"Sec-WebSocket-Key: {key}\r\n\r\n"
                ).encode()
            )
            while b"\r\n\r\n" not in self.buffer:
                self._receive()
                if len(self.buffer) > 16384:
                    raise RuntimeError("oversized WebSocket handshake")
            head, self.buffer = self.buffer.split(b"\r\n\r\n", 1)
            headers = {
                key.lower(): value
                for key, value in (
                    line.split(b":", 1) for line in head.split(b"\r\n")[1:]
                )
            }
            expected = base64.b64encode(
                hashlib.sha1(
                    (key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()
                ).digest()
            )
            if (
                not head.startswith(b"HTTP/1.1 101 ")
                or headers.get(b"sec-websocket-accept", b"").strip() != expected
            ):
                raise RuntimeError("invalid WebSocket handshake")
            self.request(
                "initialize",
                {
                    "clientInfo": {
                        "name": "agentchat-adr0028-scoped-readiness",
                        "version": "1",
                    },
                    "capabilities": {"experimentalApi": True},
                },
            )
            self.send({"method": "initialized", "params": {}})
        except Exception:
            self.close()
            raise

    def close(self):
        """Close only this probe connection."""
        self.socket.close()

    def _receive(self):
        chunk = self.socket.recv(65536)
        if not chunk:
            raise RuntimeError("daemon disconnected")
        self.buffer += chunk

    def _exact(self, count):
        while len(self.buffer) < count:
            self._receive()
        result, self.buffer = self.buffer[:count], self.buffer[count:]
        return result

    def _frame(self, payload, opcode=1):
        count = len(payload)
        mask = os.urandom(4)
        if count < 126:
            head = bytes([0x80 | opcode, 0x80 | count])
        elif count <= 65535:
            head = bytes([0x80 | opcode, 0xFE]) + struct.pack("!H", count)
        else:
            head = bytes([0x80 | opcode, 0xFF]) + struct.pack("!Q", count)
        masked = bytes(value ^ mask[index % 4] for index, value in enumerate(payload))
        self.socket.sendall(head + mask + masked)

    def send(self, value):
        """Send one JSON text frame."""
        self._frame(json.dumps(value).encode())

    def receive(self):
        """Receive one complete JSON text message."""
        fragments = []
        total = 0
        while True:
            first, second = self._exact(2)
            opcode = first & 15
            final = bool(first & 128)
            if first & 0x70 or second & 128:
                raise RuntimeError("unsupported WebSocket frame")
            count = second & 127
            if count == 126:
                count = struct.unpack("!H", self._exact(2))[0]
            elif count == 127:
                count = struct.unpack("!Q", self._exact(8))[0]
            if count > self.limit or total + count > self.limit:
                raise RuntimeError("oversized daemon message")
            if opcode >= 8 and (not final or count > 125):
                raise RuntimeError("invalid control frame")
            payload = self._exact(count)
            if opcode == 8:
                raise RuntimeError("daemon closed the WebSocket")
            if opcode == 9:
                self._frame(payload, opcode=10)
                continue
            if opcode == 10:
                continue
            if opcode != (0 if fragments else 1):
                raise RuntimeError("invalid text message fragmentation")
            fragments.append(payload)
            total += count
            if final:
                return json.loads(b"".join(fragments))

    def request(self, method, params, timeout=5):
        """Return one correlated response and ignore unrelated notifications."""
        self.counter += 1
        request_id = self.counter
        self.send({"id": request_id, "method": method, "params": params})
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.socket.settimeout(max(0.001, deadline - time.monotonic()))
            result = self.receive()
            if result.get("id") != request_id:
                continue
            if "error" in result:
                raise RpcError(result["error"])
            return result["result"]
        raise TimeoutError(f"{method} timed out")


def routing(server):
    """Select the documented experimental AgentChat routing capability only."""
    experimental = server.get("serverCapabilities", {}).get("experimental", {})
    return experimental.get("agentchat/native_routing", {})


def thread_connected(status):
    """Mirror the production decoder's supported non-disconnected states."""
    kind = status.get("type")
    if kind == "idle":
        return True
    if kind != "active" or not isinstance(status.get("activeFlags"), list):
        return False
    flags = status["activeFlags"]
    return not flags or any(
        flag in ("waitingOnApproval", "waitingOnUserInput") for flag in flags
    )


def observe(
    client,
    root,
    directory,
    expected_version,
    expected_namespace,
    expected_protocol,
):
    """Read one exact root and wait only for its scoped MCP inventory."""
    thread = client.request(
        "thread/read", {"threadId": root, "includeTurns": False}
    )["thread"]
    status = None
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        status = client.request(
            "mcpServerStatus/list",
            {
                "threadId": root,
                "serverName": "agentchat",
                "detail": "toolsAndAuthOnly",
                "limit": 1,
            },
        )
        entries = status.get("data", [])
        if entries and entries[0].get("runtimeStatus") == "connected":
            break
        time.sleep(0.25)
    entries = (status or {}).get("data", [])
    server = entries[0] if len(entries) == 1 else {}
    route = routing(server)
    tools = sorted((server.get("tools") or {}).keys())
    required_tools = ["agentchat_post", "agentchat_read", "agentchat_whoami"]
    checks = {
        "thread_id": thread.get("id") == root,
        "thread_cwd": thread.get("cwd") == directory,
        "thread_connected": thread_connected(thread.get("status", {})),
        "one_server": len(entries) == 1 and (status or {}).get("nextCursor") is None,
        "server_name": server.get("name") == "agentchat",
        "server_connected": server.get("runtimeStatus") == "connected",
        "server_version": server.get("serverInfo", {}).get("version") == expected_version,
        "tools_error_absent": server.get("toolsError") is None,
        "required_tools": all(name in tools for name in required_tools),
        "routing_host": route.get("host") == "codex",
        "routing_protocol": route.get("protocol") == expected_protocol,
        "routing_namespace": route.get("namespace") == expected_namespace,
    }
    turns_error = None
    try:
        client.request("thread/turns/list", {"threadId": root, "limit": 1})
    except RpcError as error:
        turns_error = error.value.get("code")
    return {
        "root": root,
        "thread": {
            "id": thread.get("id"),
            "cwd": thread.get("cwd"),
            "status": thread.get("status"),
        },
        "mcp": {
            "count": len(entries),
            "next_cursor": (status or {}).get("nextCursor"),
            "name": server.get("name"),
            "runtime_status": server.get("runtimeStatus"),
            "server_info": server.get("serverInfo"),
            "tools_error": server.get("toolsError"),
            "tool_names": tools,
            "routing": route,
        },
        "turns_list_error": turns_error,
        "checks": checks,
        "ready": all(checks.values()),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--socket", required=True)
    parser.add_argument("--cwd", required=True)
    parser.add_argument("--expected-version", required=True)
    parser.add_argument("--expected-namespace", required=True)
    parser.add_argument("--expected-protocol", required=True, type=int)
    parser.add_argument("--platform", required=True)
    args = parser.parse_args()
    directory = os.path.realpath(args.cwd)
    client = Codex(args.socket)
    try:
        observations = []
        for transition in ("initial", "distinct_root"):
            result = client.request("thread/start", {"cwd": directory})
            root = result["thread"]["id"]
            observation = observe(
                client,
                root,
                directory,
                args.expected_version,
                args.expected_namespace,
                args.expected_protocol,
            )
            observation["transition"] = transition
            observations.append(observation)
        distinct = observations[0]["root"] != observations[1]["root"]
        passed = distinct and all(item["ready"] for item in observations)
        print(
            json.dumps(
                {
                    "schema": "agentchat.codex-scoped-readiness/v1",
                    "platform": args.platform,
                    "same_connection": True,
                    "model_inputs": 0,
                    "mcp_tool_calls": 0,
                    "agentchat_join_calls": 0,
                    "shared_daemon_stopped": False,
                    "roots_distinct": distinct,
                    "observations": observations,
                    "passed": passed,
                },
                indent=2,
                sort_keys=True,
            )
        )
        raise SystemExit(0 if passed else 1)
    finally:
        client.close()


if __name__ == "__main__":
    main()

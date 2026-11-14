"""Read-only native observer transport derived from the retained ADR 0027 rig.

The 16 MiB cap applies only to this test observer's retained JSON message. It is
not a product proxy, a native forwarding limit or an admission policy. No native
request is answered, rewritten or privileged by this observer.
"""
import base64,hashlib,json,os,socket,struct,time

class ProtocolError(RuntimeError): pass

class Codex:
    """WebSocket JSON RPC over the existing daemon's Unix control socket."""
    LIMIT = 16 * 1024 * 1024

    def __init__(self, path):
        self.socket = socket.socket(socket.AF_UNIX)
        self.socket.settimeout(3)
        self.buffer = b""
        self.counter = 0
        self.observed = []
        try:
            self.socket.connect(str(path))
            key = base64.b64encode(os.urandom(16)).decode()
            self.socket.sendall((
                "GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n"
                "Connection: Upgrade\r\nSec-WebSocket-Version: 13\r\n"
                f"Sec-WebSocket-Key: {key}\r\n\r\n"
            ).encode())
            while b"\r\n\r\n" not in self.buffer:
                self._receive()
                if len(self.buffer) > 16384:
                    raise ProtocolError("Oversized WebSocket handshake")
            head, self.buffer = self.buffer.split(b"\r\n\r\n", 1)
            headers = {k.lower(): v for k, v in
                       (line.split(b":", 1) for line in head.split(b"\r\n")[1:])}
            expected = base64.b64encode(hashlib.sha1(
                (key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest())
            if not head.startswith(b"HTTP/1.1 101 ") or headers.get(
                    b"sec-websocket-accept", b"").strip() != expected:
                raise ProtocolError("Invalid daemon WebSocket handshake")
            initialized = self.request("initialize", {
                "clientInfo": {"name": "agentchat_adr3031_owned_observer", "version": "1"},
                "capabilities": {"experimentalApi": True},
            })
            self.send({"method": "initialized", "params": {}})
        except Exception:
            self.close()
            raise

    def close(self):
        self.socket.close()

    def _receive(self):
        chunk = self.socket.recv(65536)
        if not chunk:
            raise ProtocolError("Daemon disconnected")
        self.buffer += chunk

    def exact(self, count):
        while len(self.buffer) < count:
            self._receive()
        result, self.buffer = self.buffer[:count], self.buffer[count:]
        return result

    def frame(self, payload, opcode=1):
        count, mask = len(payload), os.urandom(4)
        if count < 126:
            head = bytes([0x80 | opcode, 0x80 | count])
        elif count <= 65535:
            head = bytes([0x80 | opcode, 0xfe]) + struct.pack("!H", count)
        else:
            head = bytes([0x80 | opcode, 0xff]) + struct.pack("!Q", count)
        self.socket.sendall(head + mask + bytes(c ^ mask[i % 4] for i, c in enumerate(payload)))

    def send(self, value):
        self.frame(json.dumps(value).encode())

    def receive(self):
        fragments = []
        total = 0
        while True:
            first, second = self.exact(2)
            opcode, final = first & 15, bool(first & 128)
            if first & 0x70 or second & 128:
                raise ProtocolError("Unsupported WebSocket frame")
            count = second & 127
            if count == 126:
                count = struct.unpack("!H", self.exact(2))[0]
            elif count == 127:
                count = struct.unpack("!Q", self.exact(8))[0]
            if count > self.LIMIT or total + count > self.LIMIT:
                raise ProtocolError("Oversized daemon message")
            if opcode >= 8 and (not final or count > 125):
                raise ProtocolError("Invalid control frame")
            payload = self.exact(count)
            if opcode == 8:
                raise ProtocolError("Daemon closed the WebSocket")
            if opcode == 9:
                self.frame(payload, opcode=10)
                continue
            if opcode == 10:
                continue
            if opcode != (0 if fragments else 1):
                raise ProtocolError("Invalid text message fragmentation")
            fragments.append(payload)
            total += count
            if final:
                return json.loads(b"".join(fragments))

    def request(self, method, params):
        self.counter += 1
        request_id = self.counter
        self.send({"id": request_id, "method": method, "params": params})
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            self.socket.settimeout(max(.001, deadline - time.monotonic()))
            result = self.receive()
            if result.get("id") != request_id:
                self.observed.append(result)
            if result.get("id") == request_id:
                if "error" in result:
                    raise ProtocolError(json.dumps(result["error"]))
                return result["result"]
        raise TimeoutError("Daemon RPC timed out")

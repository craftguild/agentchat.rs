#!/usr/bin/env python3
"""Owned PTY controller; normal terminal replies do not answer host approvals."""
import fcntl
import http.server
import json
import os
from pathlib import Path
import pty
import struct
import subprocess
import sys
import termios
import threading

ROOT = Path(sys.argv[1]).resolve()
sys.path.insert(0, '/private/var/tmp/agentchat-rust-native-463jwm2x/pydeps')
import pyte

terms = {}


class Screen(pyte.Screen):
    def __init__(self, writer):
        super().__init__(120, 40)
        self.writer = writer

    def write_process_input(self, data):
        self.writer(data)

    def report_device_status(self, mode, **kwargs):
        # pyte 0.8 lacks the private DSR argument used by this native version.
        if kwargs.get('private'):
            if mode == 6:
                self.write_process_input(f'\x1b[?{self.cursor.y + 1};{self.cursor.x + 1}R')
        else:
            super().report_device_status(mode)


class Terminal:
    def __init__(self, name, argv, cwd, environment):
        self.name = name
        self.screen = Screen(self.reply)
        self.stream = pyte.Stream(self.screen)
        self.lock = threading.Lock()
        self.query_count = 0
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 120, 0, 0))
        env = dict(os.environ, **environment, TERM='xterm-256color')
        env.pop('CLAUDECODE', None)
        self.child = subprocess.Popen(argv, cwd=cwd, env=env, stdin=slave, stdout=slave,
                                      stderr=slave, start_new_session=True)
        os.close(slave)
        self.fd = master
        threading.Thread(target=self.read, daemon=True).start()

    def reply(self, data):
        self.query_count += 1
        os.write(self.fd, data.encode())

    def read(self):
        with (ROOT / f'{self.name}.terminal.raw').open('ab', buffering=0) as output:
            while True:
                try:
                    data = os.read(self.fd, 65536)
                except OSError:
                    return
                if not data:
                    return
                output.write(data)
                with self.lock:
                    self.stream.feed(data.decode('utf-8', 'replace'))

    def view(self):
        with self.lock:
            return {'pid': self.child.pid, 'exit': self.child.poll(), 'queries': self.query_count,
                    'cursor': [self.screen.cursor.y + 1, self.screen.cursor.x + 1],
                    'screen': '\n'.join(self.screen.display)}


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        try:
            size = int(self.headers['Content-Length'])
            if not 0 < size <= 1_048_576:
                raise ValueError('Request bound')
            request = json.loads(self.rfile.read(size))
            action, name = request['op'], request.get('name')
            if action == 'start':
                if name in terms or len(terms) >= 3:
                    raise ValueError('Terminal capacity or reused name')
                terms[name] = Terminal(name, request['argv'], request['cwd'], request['env'])
                result = terms[name].view()
            elif action == 'screen':
                result = terms[name].view()
            elif action == 'input':
                os.write(terms[name].fd, request['text'].encode())
                result = {'sent': True}
            elif action == 'signal':
                terms[name].child.send_signal(request['signal'])
                result = {'sent': True}
            elif action == 'list':
                result = {name: term.view() for name, term in terms.items()}
            else:
                raise ValueError('Unknown operation')
            self.send_response(200)
        except Exception as error:
            result = {'error': str(error)}
            self.send_response(400)
        data = json.dumps(result).encode()
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)


server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
(ROOT / 'terminal-control.json').write_text(json.dumps({'pid': os.getpid(), 'port': server.server_port}))
print(json.dumps({'pid': os.getpid(), 'port': server.server_port}), flush=True)
server.serve_forever()

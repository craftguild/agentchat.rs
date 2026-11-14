"""Native measurements against known resources in a separately owned child."""

import os
from pathlib import Path
import selectors
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from macos_resources import resources

CHILD = r"""
import resource, sys, tempfile, threading
files, threads = [], []
stop = threading.Event()
print('ready', flush=True)
try:
    for command in sys.stdin:
        command = command.strip()
        if command in ('grow', 'many'):
            count = 3 if command == 'grow' else 260
            if command == 'many':
                soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
                if hard != resource.RLIM_INFINITY and hard < 512:
                    print('low-hard-limit', flush=True)
                    continue
                resource.setrlimit(resource.RLIMIT_NOFILE, (max(soft, 512), hard))
            files = [tempfile.TemporaryFile() for _ in range(count)]
            if command == 'grow':
                threads = [threading.Thread(target=stop.wait) for _ in range(2)]
                for thread in threads: thread.start()
        elif command == 'shrink':
            stop.set()
            for thread in threads: thread.join()
            for file in files: file.close()
            files, threads = [], []
            stop.clear()
        else:
            break
        print('ready', flush=True)
finally:
    stop.set()
    for thread in threads: thread.join()
    for file in files: file.close()
"""


@unittest.skipUnless(sys.platform == "darwin", "Requires native macOS libproc")
class NativeResources(unittest.TestCase):
    def test_owned_child_threads_files_and_guard_exceedance_are_measured(self):
        with tempfile.TemporaryDirectory(prefix="agentchat-native-resource-test-") as root:
            environment = dict(os.environ, AGENTCHAT_HOME=str(Path(root) / "state"),
                               AGENTCHAT_USER_CONFIG_ROOT=str(Path(root) / "config"), TMPDIR=root)
            child = subprocess.Popen([sys.executable, "-u", "-c", CHILD], stdin=subprocess.PIPE,
                                     stdout=subprocess.PIPE, text=True, env=environment)
            selector = selectors.DefaultSelector()
            selector.register(child.stdout, selectors.EVENT_READ)

            def ready():
                self.assertTrue(selector.select(timeout=5), "Owned child did not respond")
                return child.stdout.readline().strip()

            def send(command):
                child.stdin.write(command + "\n")
                child.stdin.flush()
                return ready()

            try:
                self.assertEqual(ready(), "ready")
                before = resources(child.pid)
                self.assertGreater(before["rss_kib"], 0)
                self.assertGreaterEqual(before["threads"], 1)
                self.assertEqual(send("grow"), "ready")
                grown = resources(child.pid)
                self.assertEqual(grown["threads"], before["threads"] + 2)
                self.assertEqual(grown["fds"], before["fds"] + 3)
                self.assertEqual(send("shrink"), "ready")
                restored = resources(child.pid)
                self.assertEqual(restored["threads"], before["threads"])
                self.assertEqual(restored["fds"], before["fds"])
                self.assertEqual(send("many"), "ready", "Descriptor hard limit is below 512")
                many = resources(child.pid)
                self.assertEqual(many["fds"], before["fds"] + 260)
                self.assertGreater(many["fds"], 256)
                self.assertEqual(send("shrink"), "ready")
                self.assertEqual(resources(child.pid)["fds"], before["fds"])
            finally:
                child.stdin.close()
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
                selector.close()
                child.stdout.close()
            self.assertEqual(child.returncode, 0)


if __name__ == "__main__":
    unittest.main()

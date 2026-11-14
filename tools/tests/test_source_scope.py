"""Verify the exact independent-workspace boundary with a real Git inventory."""

import importlib.util
from pathlib import Path
import runpy
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
included = runpy.run_path(ROOT / "tools/source_scope.py")["is_agentchat_file"]


class SourceScope(unittest.TestCase):
    def test_only_the_approved_workspace_is_excluded(self):
        self.assertFalse(included("experiments/web-api-poc/generated/src/model.rs"))
        for name in ["experiments/another/src/model.rs", "experiments/web-api-poc-next/a.rs",
                     "crates/agentchat-cli/src/main.rs", "tools/generated.py"]:
            self.assertTrue(included(name), name)

    def test_digest_ignores_independent_edits_but_tracks_product_and_other_experiments(self):
        spec = importlib.util.spec_from_file_location("scope_workload", ROOT / "tools/run-workload.py")
        driver = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(driver)
        with tempfile.TemporaryDirectory(prefix="agentchat-source-scope-") as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            driver.ROOT = root

            def write(name, body):
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(body)

            product = "crates/agentchat-cli/src/main.rs"
            experiment = "experiments/web-api-poc/generated/src/model.rs"
            write(product, "fn main() {}")
            subprocess.run(["git", "add", "."], cwd=root, check=True)
            initial = driver.source_digest()
            write(experiment, "independent revision one")
            self.assertEqual(driver.source_digest(), initial)
            subprocess.run(["git", "add", "."], cwd=root, check=True)
            write(experiment, "independent revision two")
            self.assertEqual(driver.source_digest(), initial)
            write(product, "fn main() { todo!() }")
            changed = driver.source_digest()
            self.assertNotEqual(changed, initial)
            write("experiments/another/generated.rs", "still covered")
            self.assertNotEqual(driver.source_digest(), changed)

    def test_unstaged_crate_move_fingerprints_the_present_successor(self):
        spec = importlib.util.spec_from_file_location("scope_workload", ROOT / "tools/run-workload.py")
        driver = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(driver)
        with tempfile.TemporaryDirectory(prefix="agentchat-source-move-") as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            driver.ROOT = root
            old = root / "crates/ac-core/src/lib.rs"
            old.parent.mkdir(parents=True)
            old.write_text("pub fn original() {}")
            subprocess.run(["git", "add", "."], cwd=root, check=True)
            initial = driver.source_digest()
            new = root / "crates/agentchat-core/src/lib.rs"
            new.parent.mkdir(parents=True)
            old.rename(new)
            moved = driver.source_digest()
            self.assertNotEqual(initial, moved)
            new.write_text("pub fn changed() {}")
            self.assertNotEqual(moved, driver.source_digest())


if __name__ == "__main__":
    unittest.main()

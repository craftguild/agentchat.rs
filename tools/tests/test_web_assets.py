"""Reject stale/extra assets and bind actual embedded bytes into workload seals."""

import importlib.util
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("web_assets", ROOT / "tools/web_assets.py")
ASSETS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ASSETS)


class Assets(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="agentchat-assets-")
        self.root = Path(self.temp.name)
        for name in ASSETS.DIRECTORIES + ["web/dist"]:
            shutil.copytree(ROOT / name, self.root / name)
        for name in ASSETS.INPUTS:
            target = self.root / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / name, target)

    def tearDown(self):
        self.temp.cleanup()

    def test_exact_owned_assets_include_ignored_runtime_files(self):
        paths = ASSETS.verify(self.root)
        self.assertIn(ASSETS.MANIFEST, paths)
        self.assertIn("web/dist/index.html", paths)
        self.assertIn("web/dist/third-party-notices.txt", paths)
        self.assertIn("web/dist/build-info.txt", paths)
        self.assertTrue(any(name.endswith(".js") for name in paths))

    def test_stale_missing_extra_and_changed_assets_refuse(self):
        cases = [("web/src/app.tsx", "append"), ("web/codegen.ts", "append"),
                 ("web/src/graphql/catalog.generated.ts", "append"), ("web/dist/index.html", "append"),
                 ("web/src/extra.ts", "extra"), ("web/dist/extra.js", "extra"),
                 ("web/dist/index.html", "missing"),
                 ("web/licenses/wryware-MIT.txt", "append"),
                 ("web/dist/third-party-notices.txt", "append"),
                 ("web/dist/build-info.txt", "append"),
                 ("Cargo.toml", "append")]
        for name, action in cases:
            with self.subTest(name=name, action=action):
                path = self.root / name
                original = path.read_bytes() if path.exists() else None
                if action == "append":
                    path.write_bytes(original + b"\n")
                elif action == "extra":
                    path.write_text("unowned")
                else:
                    path.unlink()
                with self.assertRaises(ValueError):
                    ASSETS.verify(self.root)
                if original is not None:
                    path.write_bytes(original)
                else:
                    path.unlink()

    def test_symlink_and_manifest_inventory_omission_refuse(self):
        path = self.root / "web/dist/index.html"
        path.unlink()
        path.symlink_to(ROOT / "web/dist/index.html")
        with self.assertRaises(ValueError):
            ASSETS.verify(self.root)
        path.unlink()
        shutil.copy2(ROOT / "web/dist/index.html", path)
        manifest = self.root / ASSETS.MANIFEST
        value = json.loads(manifest.read_text())
        value["inputs"].pop("web/src/app.tsx")
        manifest.write_text(json.dumps(value))
        with self.assertRaises(ValueError):
            ASSETS.verify(self.root)

    def test_workload_fingerprint_includes_ignored_embedded_bytes(self):
        spec = importlib.util.spec_from_file_location("asset_workload", ROOT / "tools/run-workload.py")
        driver = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(driver)
        driver.ROOT = self.root
        manifest = self.root / "crates/agentchat-web/Cargo.toml"
        manifest.parent.mkdir(parents=True)
        manifest.write_text('[package]\nname="asset-fixture"\n')
        (self.root / ".gitignore").write_text("web/dist/\n")
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        inventory = subprocess.check_output(
            ["git", "ls-files", "--others", "--exclude-standard"], cwd=self.root).decode()
        self.assertNotIn("web/dist/index.html", inventory)
        initial = driver.source_digest()
        path = self.root / "web/dist/index.html"
        path.write_bytes(path.read_bytes() + b"\n")
        seal = self.root / ASSETS.MANIFEST
        value = json.loads(seal.read_text())
        for asset in value["assets"]:
            if asset["path"] == "index.html":
                asset.update(bytes=path.stat().st_size, sha256=hashlib.sha256(path.read_bytes()).hexdigest())
        seal.write_text(json.dumps(value))
        self.assertNotEqual(initial, driver.source_digest())

    def test_notice_cannot_be_removed_even_with_a_consistent_asset_inventory(self):
        seal = self.root / ASSETS.MANIFEST
        value = json.loads(seal.read_text())
        value["assets"] = [asset for asset in value["assets"] if asset["path"] != "third-party-notices.txt"]
        seal.write_text(json.dumps(value))
        (self.root / "web/dist/third-party-notices.txt").unlink()
        with self.assertRaises(ValueError):
            ASSETS.verify(self.root)

    def test_build_info_cannot_be_removed_even_with_a_consistent_asset_inventory(self):
        seal = self.root / ASSETS.MANIFEST
        value = json.loads(seal.read_text())
        value["assets"] = [asset for asset in value["assets"] if asset["path"] != "build-info.txt"]
        seal.write_text(json.dumps(value))
        (self.root / "web/dist/build-info.txt").unlink()
        with self.assertRaises(ValueError):
            ASSETS.verify(self.root)


if __name__ == "__main__":
    unittest.main()

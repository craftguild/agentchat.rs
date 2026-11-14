"""Reject manual code or stale/changed inputs before granting a size exemption."""

import importlib.util
import json
import pathlib
import shutil
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("contract", ROOT / "tools/chigiri_contract.py")
CONTRACT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CONTRACT)


class ContractTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="agentchat-contract-")
        self.root = pathlib.Path(self.temp.name)
        for name in ["web/graphql", "generated/agentchat-viewer-contract"]:
            shutil.copytree(ROOT / name, self.root / name)
        shutil.copy2(ROOT / "web/chigiri-provenance.json", self.root / "web/chigiri-provenance.json")
        shutil.copy2(ROOT / "chigiri.toml", self.root / "chigiri.toml")

    def tearDown(self):
        self.temp.cleanup()

    def test_only_owned_rust_is_exempt(self):
        paths = CONTRACT.verify(self.root)
        self.assertGreater(len(paths), 0)
        self.assertTrue(all(p.startswith(CONTRACT.OUTPUT + "/src/") and p.endswith(".rs")
                            for p in paths))
        self.assertNotIn(CONTRACT.OUTPUT + "/Cargo.toml", paths)

    def test_changed_manual_extra_missing_and_symlink_sources_refuse(self):
        path = self.root / CONTRACT.OUTPUT / "src/lib.rs"
        original = path.read_bytes()
        for action in ["change", "extra", "missing", "symlink"]:
            with self.subTest(action=action):
                extra = path.parent / "manual.rs"
                if action == "change":
                    path.write_bytes(original + b"\nfn manual() {}\n")
                elif action == "extra":
                    extra.write_text("fn manual() {}\n")
                elif action == "missing":
                    path.unlink()
                else:
                    path.unlink()
                    path.symlink_to(ROOT / CONTRACT.OUTPUT / "src/lib.rs")
                with self.assertRaises(ValueError):
                    CONTRACT.verify(self.root)
                if extra.exists():
                    extra.unlink()
                if path.is_symlink():
                    path.unlink()
                path.write_bytes(original)

    def test_stale_manifest_schema_input_seal_and_ownership_refuse(self):
        for name in ["chigiri.toml", "web/graphql/schema.graphql",
                     CONTRACT.OUTPUT + "/chigiri-generated.json"]:
            with self.subTest(name=name):
                path = self.root / name
                original = path.read_bytes()
                path.write_bytes(original + b"\n")
                with self.assertRaises(ValueError):
                    CONTRACT.verify(self.root)
                path.write_bytes(original)

    def test_unknown_schema_input_and_unqualified_generator_refuse(self):
        extra = self.root / "web/graphql/manual.graphql"
        extra.write_text("type Extra { value: String }")
        with self.assertRaises(ValueError):
            CONTRACT.verify(self.root)
        extra.unlink()
        with self.assertRaisesRegex(ValueError, "unqualified generator"):
            CONTRACT.regenerate(self.root, self.root / "chigiri.toml")

    def test_changed_published_artifact_refuses(self):
        path = self.root / "web/chigiri-provenance.json"
        seal = json.loads(path.read_text())
        seal["release_artifacts"]["aarch64-apple-darwin"] = "0" * 64
        path.write_text(json.dumps(seal))
        with self.assertRaisesRegex(ValueError, "published generator artifacts"):
            CONTRACT.verify(self.root)

    def test_alternate_abi_revision_and_dirty_generator_refuse(self):
        path = self.root / "web/chigiri-provenance.json"
        original = json.loads(path.read_text())
        for key, value in [("abi", 7), ("source_revision", "0" * 40)]:
            changed = dict(original)
            changed[key] = value
            path.write_text(json.dumps(changed))
            with self.assertRaises(ValueError):
                CONTRACT.verify(self.root)
        path.write_text(json.dumps(original))
        changed = dict(original)
        changed["generator"] = dict(original["generator"], dirty=True)
        path.write_text(json.dumps(changed))
        with self.assertRaises(ValueError):
            CONTRACT.verify(self.root)


if __name__ == "__main__":
    unittest.main()

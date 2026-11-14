"""Verify owned frontend inputs and include ignored embedded assets in engineering seals."""

import hashlib
import json
from pathlib import Path, PurePosixPath

DIRECTORIES = ["web/src", "web/public", "web/graphql", "web/fixtures", "web/scripts", "web/licenses"]
INPUTS = ["Cargo.toml", "web/package.json", "web/package-lock.json", "web/tsconfig.json", "web/vite.config.ts",
          "web/codegen.ts",
          "web/index.html", "web/chigiri-provenance.json", "chigiri.toml",
          "generated/agentchat-viewer-contract/chigiri-generated.json"]
MANIFEST = "web/dist/agentchat-assets.json"
MIME = {".html": "text/html; charset=utf-8", ".js": "text/javascript; charset=utf-8",
        ".css": "text/css; charset=utf-8", ".svg": "image/svg+xml", ".woff2": "font/woff2",
        ".txt": "text/plain; charset=utf-8"}


def require(condition, message):
    if not condition:
        raise ValueError("Embedded viewer assets: " + message)


def file(root, name):
    relative = PurePosixPath(name)
    require(not relative.is_absolute() and ".." not in relative.parts
            and name == relative.as_posix(), "invalid path")
    path = root / name
    require(path.is_file() and not path.is_symlink() and path.resolve() == path, "nonregular/escaped " + name)
    return path


def inventory(root, name):
    directory = root / name
    require(directory.is_dir() and directory.resolve() == directory, "missing/escaped " + name)
    paths = set()
    for path in directory.rglob("*"):
        require(not path.is_symlink(), "symlink in " + name)
        if path.is_file():
            paths.add(path.relative_to(root).as_posix())
        else:
            require(path.is_dir(), "nonregular entry in " + name)
    return paths


def verify(root):
    """Return manifest and every exact asset, even though dist is Git-ignored.

    The actual source inventory must match; a self-reported hash list cannot
    silently omit new inputs, add manual assets or import a sibling workspace.
    """
    root = root.resolve()
    path = file(root, MANIFEST)
    manifest = json.loads(path.read_text())
    require(set(manifest) == {"schemaVersion", "node", "inputs", "assets"}
            and manifest["schemaVersion"] == 1 and manifest["node"].startswith("v"), "schema/build identity")
    expected = set(INPUTS)
    for name in DIRECTORIES:
        expected.update(inventory(root, name))
    require(expected == set(manifest["inputs"]), "input inventory mismatch; rebuild assets")
    for name, digest in manifest["inputs"].items():
        require(hashlib.sha256(file(root, name).read_bytes()).hexdigest() == digest,
                "stale input " + name + "; rebuild assets")
    assets = manifest["assets"]
    require(0 < len(assets) <= 64, "asset count budget")
    actual = inventory(root, "web/dist") - {MANIFEST}
    owned, total = set(), 0
    for asset in assets:
        require(set(asset) == {"path", "mime", "bytes", "sha256"}, "unknown asset fields")
        name = "web/dist/" + asset["path"]
        require(name not in owned, "duplicate asset")
        owned.add(name)
        data = file(root, name).read_bytes()
        require(len(data) == asset["bytes"] and len(data) <= 1024 * 1024
                and hashlib.sha256(data).hexdigest() == asset["sha256"], "changed/oversized asset " + name)
        require(MIME.get(Path(name).suffix) == asset["mime"], "asset MIME mismatch")
        total += len(data)
    require(owned == actual and {"web/dist/index.html", "web/dist/third-party-notices.txt", "web/dist/build-info.txt"} <= owned
            and total <= 8 * 1024 * 1024,
            "asset ownership/byte budget")
    return owned | {MANIFEST}


if __name__ == "__main__":
    paths = verify(Path(__file__).resolve().parent.parent)
    print(f"PASS: frontend inputs and {len(paths) - 1} exact embedded assets")

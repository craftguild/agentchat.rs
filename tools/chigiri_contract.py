"""Verify published Chigiri provenance and adapt its owned output to Standards."""

import argparse
import hashlib
import json
import pathlib
import platform
import shutil
import subprocess
import tempfile

OUTPUT = "generated/agentchat-viewer-contract"
REVISION = "5f90235ddb705a1a394b9a5cfda23a9ea62dc4b6"
VERSION = "0.1.1"
ARTIFACTS = {
    "aarch64-apple-darwin": "de1690d68fec85eadb2d51f934d0546a3b9882f5d753203fc2cc3a9d3f4861ed",
    "x86_64-unknown-linux-gnu": "fbdd66d4778ff7e3366b7db41e10243279c28015678160263bb55f6754c00fe7",
}
RUNTIME = {"source": "git", "url": "https://github.com/craftguild/chigiri.rs.git",
           "revision": REVISION}


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def require(condition, message):
    if not condition:
        raise ValueError("Chigiri contract: " + message)


def safe_file(root, relative):
    path = pathlib.PurePosixPath(relative)
    require(not path.is_absolute() and ".." not in path.parts, "invalid owned path")
    target = root / path
    require(target.is_file() and not target.is_symlink(), "missing/nonregular " + relative)
    require(target.resolve().is_relative_to(root.resolve()), "escaped owned path")
    return target


def verify_records(root, records):
    paths = set()
    for row in records:
        name = row["path"]
        require(name not in paths, "duplicate owned path")
        paths.add(name)
        path = safe_file(root, name)
        require(path.stat().st_size == row["bytes"] and digest(path) == row["sha256"],
                "changed bytes: " + name)
    return paths


def verify(root):
    """Only exact generated Rust with a clean published producer gets an exception."""
    seal = json.loads((root / "web/chigiri-provenance.json").read_text())
    require(seal["schema_version"] == 3 and seal["output"] == OUTPUT
            and seal["source_kind"] == "published-release"
            and seal["source_revision"] == REVISION and seal["abi"] == 8,
            "unreviewed producer/ABI")
    require(seal["release_artifacts"] == ARTIFACTS, "changed published generator artifacts")
    directory = root / OUTPUT
    marker = safe_file(directory, "chigiri-generated.json")
    require(digest(marker) == seal["ownership_sha256"], "changed ownership record")
    owned = json.loads(marker.read_text())
    require(owned["schema_version"] == "chigiri/generated-crate-v1"
            and owned["package"] == {"name": seal["package"], "version": "0.1.0", "edition": "2024"}
            and owned["abi"] == seal["abi"], "wrong generated package/ABI")
    require(owned["runtime"] == RUNTIME and owned["generator"] == seal["generator"],
            "generator/runtime provenance mismatch")
    info = owned["generator"]
    require(info["dirty"] is False and info["git_commit"] == REVISION
            and info["revision"] == REVISION and info["version"] == VERSION,
            "generator is not the clean published revision")
    provenance = owned["provenance"]
    require(provenance["manifest_label"] == seal["manifest"]
            and digest(safe_file(root, seal["manifest"])) == provenance["manifest_sha256"],
            "changed generation manifest")
    schema = root / seal["schema_directory"]
    schema_paths = verify_records(schema, provenance["sources"])
    require(schema_paths == {p.relative_to(schema).as_posix() for p in schema.rglob("*")
                             if p.is_file()}, "unrecorded schema input")
    paths = verify_records(directory, owned["files"])
    require({"Cargo.toml", "src/lib.rs", "src/__chigiri/generator_info.rs"} <= paths,
            "incomplete generated crate")
    for path in directory.rglob("*"):
        require(not path.is_symlink(), "symlink in generated tree")
        if path.is_file():
            require(path.relative_to(directory).as_posix() in paths | {marker.name},
                    "unowned generated file: " + str(path))
    for name in paths:
        require(name == "Cargo.toml" or (name.startswith("src/") and name.endswith(".rs")),
                "unexpected generated asset")
    return {(pathlib.PurePosixPath(OUTPUT) / name).as_posix() for name in paths
            if name.endswith(".rs")}


def generator(root, setup=False):
    host = {("Darwin", "arm64"): "aarch64-apple-darwin",
            ("Linux", "x86_64"): "x86_64-unknown-linux-gnu"}.get(
                (platform.system(), platform.machine()))
    require(host in ARTIFACTS, "unsupported generator host")
    path = root / ".craftguild/tools/chigiri" / REVISION / host / "chigiri"
    if setup and not path.exists():
        path.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=path.parent) as temporary:
            stage = pathlib.Path(temporary) / "chigiri"
            url = f"https://github.com/craftguild/chigiri.rs/releases/download/v{VERSION}/chigiri-v{VERSION}-{host}"
            subprocess.run(["curl", "--disable", "-fsSL", url, "-o", str(stage)], check=True)
            require(digest(stage) == ARTIFACTS[host], "downloaded generator digest mismatch")
            stage.chmod(0o755)
            stage.replace(path)
    require(path.is_file() and not path.is_symlink(), "missing generator; run make setup")
    require(digest(path) == ARTIFACTS[host], "unqualified generator")
    return path


def regenerate(root, executable):
    require(digest(executable) in ARTIFACTS.values(), "unqualified generator binary")
    subprocess.run([str(executable.resolve()), "rust", "check", "--manifest",
                    "chigiri.toml", "--deny-warnings"], cwd=root, check=True)
    verify(root)


def record(root):
    """Record only metadata emitted by the pinned published producer after generation."""
    owned = json.loads((root / OUTPUT / "chigiri-generated.json").read_text())
    seal = {"schema_version": 3, "package": "agentchat-viewer-contract", "output": OUTPUT,
            "abi": 8, "source_revision": REVISION, "source_kind": "published-release",
            "release_artifacts": ARTIFACTS, "generator": owned["generator"],
            "ownership_sha256": digest(root / OUTPUT / "chigiri-generated.json"),
            "manifest": "chigiri.toml", "schema_directory": "web/graphql"}
    (root / "web/chigiri-provenance.json").write_text(json.dumps(seal, indent=2) + "\n")
    verify(root)


def ownership(root):
    verify(root)
    marker = f"{OUTPUT}/chigiri-generated.json"
    owned = json.loads((root / marker).read_text())
    inputs = ["chigiri.toml", "web/chigiri-provenance.json", marker]
    inputs += ["web/graphql/" + row["path"] for row in owned["provenance"]["sources"]]
    files = {f"{OUTPUT}/{row['path']}": row["sha256"] for row in owned["files"]}
    files[marker] = digest(root / marker)
    result = {"schema_version": 1, "owner": "agentchat-viewer-contract",
              "generator_revision": REVISION,
              "inputs": {name: digest(root / name) for name in inputs}, "files": files}
    path = root / ".craftguild/viewer-contract.json"
    path.parent.mkdir(exist_ok=True)
    path.write_text(json.dumps(result, indent=2) + "\n")


def reproduce(root, output):
    """Chigiri check independently renders with recorded metadata before export.

    An isolated copy retains the canonical manifest label and original GeneratorInfo,
    allowing the published Linux producer to reproduce macOS-authored output exactly.
    """
    verify(root)
    require(output.is_dir() and not output.is_symlink() and not any(output.iterdir()),
            "reproduction needs an empty directory")
    executable = generator(root)
    with tempfile.TemporaryDirectory(prefix="agentchat-contract-") as temporary:
        isolated = pathlib.Path(temporary)
        shutil.copy2(root / "chigiri.toml", isolated / "chigiri.toml")
        shutil.copytree(root / "web/graphql", isolated / "web/graphql")
        shutil.copytree(root / OUTPUT, isolated / OUTPUT)
        subprocess.run([str(executable), "rust", "check", "--manifest", "chigiri.toml",
                        "--deny-warnings"], cwd=isolated, check=True)
        verify(root)
        shutil.copytree(isolated / OUTPUT, output, dirs_exist_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--setup", action="store_true")
    parser.add_argument("--record", action="store_true")
    parser.add_argument("--ownership", action="store_true")
    parser.add_argument("--generator", type=pathlib.Path)
    parser.add_argument("--reproduce", type=pathlib.Path)
    args = parser.parse_args()
    root = pathlib.Path(__file__).resolve().parent.parent
    if args.setup:
        print(generator(root, setup=True))
        return
    if args.record:
        record(root)
    if args.ownership:
        ownership(root)
    if args.generator:
        regenerate(root, args.generator)
    if args.reproduce:
        reproduce(root, args.reproduce)
    print(f"PASS: {len(verify(root))} provenance-verified Chigiri Rust sources; ABI 8")


if __name__ == "__main__":
    main()

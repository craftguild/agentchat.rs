#!/usr/bin/env python3
"""Verify product contracts beyond the shared Standards quality policies."""

import os
import pathlib
import runpy
import subprocess
import sys

from chigiri_contract import verify as verify_chigiri
from web_assets import verify as verify_assets
from orm_boundary import verify_orm

ROOT = pathlib.Path(__file__).resolve().parent.parent
included = runpy.run_path(ROOT / "tools/source_scope.py")["is_agentchat_file"]


def run(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True)


def main():
    generated = verify_chigiri(ROOT)
    assets = verify_assets(ROOT)
    print(run(sys.executable, "tools/generate-initial-migration.py", "--check"), end="")
    names = run("git", "ls-files", "--cached", "--others", "--exclude-standard").splitlines()
    sources = sorted({name for name in names if name.endswith(".rs")
                      and (ROOT / name).is_file() and included(name)})
    for name in names:
        if included(name) and name.endswith(".sql") and (ROOT / name).exists():
            raise ValueError("SQL source bypasses the product SeaORM-only contract: " + name)
    verify_orm(ROOT, sources)
    policy = [name for name in sources if (
        name.startswith("crates/agentchat-core/src/")
        or name.startswith("crates/agentchat-runtime/src/daemon/delivery/")
        or name in {"crates/agentchat-runtime/src/daemon/delivery.rs",
                    "crates/agentchat-runtime/src/daemon/coordinator/native.rs"})]
    flags = ["--target", os.environ["CRAFTGUILD_TARGET"]] if os.environ.get("CRAFTGUILD_TARGET") else []
    print(run("cargo", "run", "--quiet", "--locked", *flags, "-p", "agentchat-core",
              "--example", "check-host-boundaries", "--", *policy), end="")
    print(f"PASS: {len(generated)} provenance-verified generated Rust sources")
    print(f"PASS: {len(assets) - 1} embedded assets and complete frontend input provenance")
    print(f"PASS: frozen migration baseline and native-policy AST; {len(policy)} sources")


if __name__ == "__main__":
    main()

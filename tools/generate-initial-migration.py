#!/usr/bin/env python3
"""Freeze the initial SeaORM models and generate its immutable schema registry."""

import argparse
import hashlib
import json
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
STORE = ROOT / "crates/agentchat-store/src"
MIGRATIONS = STORE / "migrations"
FROZEN = MIGRATIONS / "initial_entities"
MANIFEST = MIGRATIONS / "initial-models.json"
REGISTRY = MIGRATIONS / "v1_tables.rs"


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--create", action="store_true", help="Create the first baseline once")
    parser.add_argument("--check", action="store_true", help="Check frozen outputs, allowing later live-model changes")
    args = parser.parse_args()
    if args.create == args.check:
        parser.error("select exactly one of --create or --check")
    if args.check:
        manifest = json.loads(MANIFEST.read_text())
        for name, expected in manifest["outputs"].items():
            path = MIGRATIONS / name
            if digest(path) != expected:
                sys.exit(f"Frozen initial migration model changed: {path}")
        expected = {name for name in manifest["outputs"] if name.startswith("initial_entities/")}
        actual = {str(p.relative_to(MIGRATIONS)) for p in FROZEN.glob("*.rs")}
        if expected != actual:
            sys.exit("Frozen initial model set differs from its generation manifest")
        print(f"PASS: {len(manifest['inputs'])} frozen initial models and generated SeaORM schema registry")
        return
    if MANIFEST.exists() or FROZEN.exists():
        sys.exit("Initial baseline already exists. Add a named SeaORM migration; do not regenerate history.")
    FROZEN.mkdir()
    inputs = {}
    models = []
    for source in sorted((STORE / "entities").glob("*.rs")):
        match = re.search(r'#\[sea_orm\(table_name = "([a-z_]+)"\)\]', source.read_text())
        if not match:
            continue
        table = match.group(1)
        module = source.stem
        inputs[str(source.relative_to(ROOT))] = digest(source)
        models.append((module, table))
        (FROZEN / source.name).write_text(
            "// Generated once from the initial SeaORM entity model. Do not edit released history.\n"
            + source.read_text())
    (FROZEN / "mod.rs").write_text(
        "//! Frozen initial entity models used only by the first historical migration.\n\n"
        + "\n".join(f"pub(crate) mod {module};" for module, _ in models) + "\n")
    registry = [
        "//! Generated initial schema registry: model-derived columns and primary keys.",
        "//! Later model changes require another named MigrationTrait implementation.",
        "", "use sea_orm::{DbBackend,Schema,sea_query::TableCreateStatement};",
        "use sea_orm_migration::DbErr;", "use super::initial_entities;", "",
        "pub(crate) fn table(name:&str)->Result<TableCreateStatement,DbErr> {",
        "let schema=Schema::new(DbBackend::Sqlite);", "let table=match name {",
    ]
    registry += [f'"{table}"=>schema.create_table_from_entity(initial_entities::{module}::Entity),'
                 for module, table in models]
    registry += [
        '_=>return Err(DbErr::Custom("Unknown initial model table.".into())),',
        "};Ok(table)", "}", "",
        "pub(crate) const MODEL_SOURCE:&str=concat!(",
    ]
    registry += [f'include_str!("initial_entities/{module}.rs"),' for module, _ in models]
    registry += [
        ");", "", "// Source artifacts stay below the ordinary production code-line limit.",
    ]
    REGISTRY.write_text("\n".join(registry) + "\n")
    outputs = sorted(FROZEN.glob("*.rs")) + [REGISTRY]
    subprocess.run(["rustfmt", "--edition", "2024", *map(str, outputs)], cwd=ROOT, check=True)
    manifest = {
        "version": 1, "migration": "m20261015_000001_initial", "sea_orm_version": "2.0.4",
        "inputs": inputs,
        "outputs": {str(p.relative_to(MIGRATIONS)): digest(p) for p in outputs},
    }
    MANIFEST.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    print(f"Created one-time initial baseline from {len(models)} SeaORM models")


if __name__ == "__main__":
    main()

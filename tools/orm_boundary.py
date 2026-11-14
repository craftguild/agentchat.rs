"""Retain the original SeaORM guard where SDK 0.6.2 rejects legal reexports.

The common policy currently treats sea_orm::sea_query as a direct driver.
Frozen released migrations cannot be rewritten to satisfy that false positive.
This keeps the original product raw/driver/schema-sync checks until upstream fixes it.
"""
import json
import re
import subprocess

RAW_API = re.compile(
    r"\b(?:execute_raw|execute_unprepared|query_one_raw|query_all_raw|"
    r"from_raw_sql|from_sql_and_values|from_sql_and_values_with_timezone|"
    r"get_sqlite_connection_pool|get_rusqlite_connection)\s*\("
    r"|\bStatement\s*::|\bExpr\s*::\s*cust\w*\s*\("
    r"|\b(?:sqlx|rusqlite|libsqlite3_sys)\s*::"
    r"|\b(?:raw_sql|raw_query|query_as|query_scalar)\s*!?\s*\("
)



def verify_orm(root, sources):
    errors = []
    for name in sources:
        if RAW_API.search((root / name).read_text()):
            errors.append("Forbidden raw/driver database query API: " + name)
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"],
        cwd=root, text=True))
    for package in metadata["packages"]:
        name = package["name"]
        for dependency in package["dependencies"]:
            target = dependency["name"]
            if target in {"sqlx", "rusqlite", "libsqlite3-sys", "sea-query"}:
                errors.append(f"Direct driver/query dependency bypasses SeaORM: {name} -> {target}")
            if target.startswith("sea-orm") and name != "agentchat-store":
                errors.append(f"ORM belongs only in agentchat-store: {name} -> {target}")
            if target == "sea-orm" and "schema-sync" in dependency["features"]:
                errors.append("Automatic model sync bypasses migration history: " + name)
    if errors:
        raise ValueError("\n".join(errors))
    print("PASS: original SeaORM-only raw/driver/schema-sync boundary, including tests")

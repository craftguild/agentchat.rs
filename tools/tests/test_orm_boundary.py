"""Preserve actual ORM policy when the shared SDK rejects legal reexports."""

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("orm_boundary", ROOT / "tools/orm_boundary.py")
ORM = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ORM)


class OrmBoundary(unittest.TestCase):
    def verify(self, source, dependency="sea-orm", features=None, package="agentchat-store"):
        metadata = {"packages": [{"name": package, "dependencies": [
            {"name": dependency, "features": features or []}]}]}
        with tempfile.TemporaryDirectory(prefix="agentchat-orm-policy-") as directory:
            root = Path(directory)
            (root / "query.rs").write_text(source)
            with patch.object(ORM.subprocess, "check_output", return_value=json.dumps(metadata)):
                ORM.verify_orm(root, ["query.rs"])

    def test_official_seaorm_seaquery_reexport_is_permitted(self):
        self.verify("use sea_orm::sea_query::{Expr, Query};")

    def test_raw_statement_and_direct_query_calls_are_rejected(self):
        for source in ["Statement::from_sql_and_values(x, y, z)",
                       "connection.execute_unprepared(sql)", "sqlx::query(sql)"]:
            with self.subTest(source=source), self.assertRaises(ValueError):
                self.verify(source)

    def test_direct_driver_or_orm_outside_store_is_rejected(self):
        for dependency in ["sqlx", "rusqlite", "sea-query"]:
            with self.subTest(dependency=dependency), self.assertRaises(ValueError):
                self.verify("", dependency=dependency)
        with self.assertRaises(ValueError):
            self.verify("", package="agentchat-web")

    def test_schema_sync_cannot_replace_frozen_migrations(self):
        with self.assertRaises(ValueError):
            self.verify("", features=["schema-sync"])

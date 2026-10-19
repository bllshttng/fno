"""Unit tests for the schema-grounded-discovery additions to db-schema.py.

Covers US2 (.env connection discovery + precedence) and US4 (migration-parser
extension to tables + columns + PRIMARY KEY + FOREIGN KEY). The live-DB path is
not exercised here (no Postgres in CI); the parser and the pure connection-
candidate resolver are the offline-testable core.

db-schema.py is a dash-named standalone script shipped as package data beside
the codemap wrapper, so it is loaded via importlib rather than imported as a
package module.
"""
from __future__ import annotations

import importlib.util
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[3]
DB_SCHEMA_PATH = REPO_ROOT / "cli" / "src" / "fno" / "codemap_cli" / "db-schema.py"


def _load_db_schema():
    spec = importlib.util.spec_from_file_location("db_schema_under_test", DB_SCHEMA_PATH)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


db = _load_db_schema()


# --- US2: connection candidate resolution (.env discovery + precedence) ---


def test_shell_database_url_wins_over_env_local(tmp_path):
    """AC2-FR: an explicit shell DATABASE_URL outranks .env.local."""
    (tmp_path / ".env.local").write_text("DATABASE_URL=postgresql://local/db\n")
    cands = db.gather_connection_candidates(str(tmp_path), {"DATABASE_URL": "postgresql://shell/db"})
    assert cands[0] == ("shell DATABASE_URL", "postgresql://shell/db")
    # .env.local is still present as a lower-precedence candidate.
    assert (".env.local", "postgresql://local/db") in cands
    assert cands.index(("shell DATABASE_URL", "postgresql://shell/db")) < cands.index(
        (".env.local", "postgresql://local/db")
    )


def test_production_env_is_never_a_candidate(tmp_path):
    """AC2-EDGE: .env.production is excluded from auto-discovery."""
    (tmp_path / ".env.production").write_text("DATABASE_URL=postgresql://prod/db\n")
    (tmp_path / ".env.staging").write_text("DATABASE_URL=postgresql://staging/db\n")
    cands = db.gather_connection_candidates(str(tmp_path), {})
    conns = [c for _, c in cands]
    assert "postgresql://prod/db" not in conns
    assert "postgresql://staging/db" not in conns
    # Only the localhost probes remain.
    assert [s for s, _ in cands] == ["localhost:54322", "localhost:5432"]


# --- US4: migration parser extension (tables + columns + PK + FK) ---


def test_tables_columns_and_primary_keys(tmp_path):
    """AC4-HP: table names, columns, and primary keys are parsed."""
    (tmp_path / "001.sql").write_text(
        """
        CREATE TABLE users (
          id uuid PRIMARY KEY,
          email text NOT NULL,
          name text
        );
        CREATE TABLE orders (
          id uuid,
          user_id uuid,
          total numeric(10,2),
          PRIMARY KEY (id),
          FOREIGN KEY (user_id) REFERENCES users (id)
        );
        """
    )
    sd = db.extract_migrations(str(tmp_path))
    assert set(sd.tables["users"]) >= {"id", "email", "name"}
    assert sd.pks["users"] == ["id"]
    assert "total" in sd.tables["orders"]
    assert sd.pks["orders"] == ["id"]
    # FK survives even though numeric(10,2) carries an inner paren.
    assert ("orders", "user_id", "users") in sd.fks


def test_malformed_ddl_tolerated(tmp_path):
    """AC4-ERR: a truncated CREATE TABLE does not crash the parser."""
    (tmp_path / "001.sql").write_text(
        """
        CREATE TYPE mood AS ENUM ('happy', 'sad');
        CREATE TABLE broken (
          id uuid PRIMARY KEY,
        """
    )
    sd = db.extract_migrations(str(tmp_path))
    assert "mood" in sd.enums  # the valid enum still parsed
    assert "broken" not in sd.tables  # the broken table was skipped


def test_autodetect_migrations_under_project_root(tmp_path):
    """The directory-positional path resolves supabase/migrations under the
    given root, not the process CWD (how `fno doctor codemap --db-schema` invokes it)."""
    mig = tmp_path / "supabase" / "migrations"
    mig.mkdir(parents=True)
    (mig / "001_init.sql").write_text("CREATE TABLE gadgets (id uuid PRIMARY KEY, label text);\n")
    sd = db.extract_migrations(None, root=str(tmp_path))
    assert "gadgets" in sd.tables
    assert "label" in sd.tables["gadgets"]
    assert sd.source_note == "migration files"


# --- Output formatting ---


def test_markdown_includes_tables_section(tmp_path):
    (tmp_path / "001.sql").write_text("CREATE TABLE widgets (id uuid PRIMARY KEY, label text);\n")
    sd = db.extract_migrations(str(tmp_path))
    md = db.format_markdown(sd)
    assert "### Tables" in md
    assert "widgets" in md
    assert "label" in md


def test_live_table_rows_normalized_before_filter():
    """A live TABLE_QUERY row is `schema.table`; an unqualified --tables filter
    must still match after schema-qualifier normalization (codex P2 on #509)."""
    rows = [("public.users", "id"), ("public.orders", "total")]
    normalized = db._normalize_table_rows(rows)
    assert ("users", "id") in normalized
    assert ("orders", "total") in normalized
    # An unqualified table filter now matches the normalized bare name.
    assert db.filter_by_tables(normalized, "users") == [("users", "id")]

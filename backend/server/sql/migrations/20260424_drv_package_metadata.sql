-- Add package metadata columns to Drv to support package change summaries
-- and rebuild impact analysis.
--
-- All columns are nullable: rows pre-dating this migration simply lack
-- metadata, and renderers fall back to the extract_pname heuristic on the
-- drv path. Metadata is sourced from `nix-eval-jobs --meta` JSONL output
-- and normalized into structured JSON columns.

-- pname/version extracted from `meta.pname`/`meta.version` when set by the
-- package author; otherwise heuristically split from the `name` field
-- (e.g. "hello-2.12.1" -> pname="hello", version="2.12.1").
ALTER TABLE Drv ADD COLUMN pname TEXT;
ALTER TABLE Drv ADD COLUMN version TEXT;

-- Normalized JSON list of license entries.
-- Each element: {"spdxId"?, "shortName"?, "fullName"?, "free"?}
-- A bare string in upstream meta is normalized to a single-entry list
-- with shortName populated.
ALTER TABLE Drv ADD COLUMN license_json TEXT;

-- Normalized JSON list of maintainer entries.
-- Each element: {"github"?, "name"?, "email"?}
ALTER TABLE Drv ADD COLUMN maintainers_json TEXT;

-- "file:line" reference to the meta declaration site (from meta.position),
-- used to link changed packages back to source.
ALTER TABLE Drv ADD COLUMN meta_position TEXT;

-- Package marked as broken / insecure by upstream meta.
ALTER TABLE Drv ADD COLUMN broken BOOLEAN;
ALTER TABLE Drv ADD COLUMN insecure BOOLEAN;

-- Index pname for rename detection and cross-jobset lookups during diff.
CREATE INDEX IF NOT EXISTS idx_drv_pname ON Drv(pname);

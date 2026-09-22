-- NotAnotherWiki Engine, 0003: role vocabulary, page lifecycle, search index.
--
-- Authentication landed in 0002 but nothing enforced anything: every visitor
-- could create and edit, and every revision was written with author_id NULL.
-- This migration adds the storage that the permission layer, the history view
-- and the search box need. The rules themselves live in Rust, not in CHECK
-- constraints, because a wiki owner has to be able to change them without a
-- migration.

-- global_role was free text with a default. A typo like 'admln' would have
-- silently dropped someone to no privileges, and 'Root' would not have matched
-- 'root'. Pin the vocabulary.
--
--   registered  a normal account, privileges come from wiki_memberships
--   staff       trusted across every wiki on this install, read and moderate
--   root        the operator of the install, every capability everywhere
ALTER TABLE users
  ADD CONSTRAINT users_global_role_check
  CHECK (global_role IN ('registered', 'staff', 'root'));

-- Soft delete. A wiki must never lose history to a click, so deleting a page
-- archives it and an admin can restore it. The unique slug index from 0001
-- stays in force on purpose: a deleted slug is still taken, and a restore
-- lands back on its original address instead of colliding with a new page.
ALTER TABLE pages ADD COLUMN deleted_at TIMESTAMPTZ NULL;
ALTER TABLE pages ADD COLUMN deleted_by UUID NULL REFERENCES users(id) ON DELETE SET NULL;

-- Readers only ever ask for live pages, so the hot lookup gets its own
-- partial index rather than filtering rows out of the full one.
CREATE INDEX pages_live_idx
  ON pages (wiki_id, namespace, slug)
  WHERE deleted_at IS NULL;

-- Which text search configuration produced search_vector.
--
-- Postgres stems at write time, so a vector built under 'russian' returns
-- nonsense when queried under 'english'. The engine picks the configuration
-- from the wiki locale and records it here, which means a reindex can find
-- every row that drifted after a wiki changed its language instead of
-- silently returning worse results forever. Stored as text and cast to
-- regconfig in queries: regconfig has no sqlx mapping.
ALTER TABLE pages ADD COLUMN search_lang TEXT NOT NULL DEFAULT 'simple';

-- Typo and substring tolerance, on top of the GIN tsvector index from 0001.
-- The tsvector matches whole stemmed words and nothing else, so it cannot
-- find "filian" from "filan" or from "ili". A wiki search box gets both of
-- those constantly, and pg_trgm is already installed.
CREATE INDEX pages_title_trgm_idx ON pages USING gin (title gin_trgm_ops);

-- The admin panel reads the audit log two ways: recent activity on one wiki,
-- and everything one account did. Both were sequential scans.
CREATE INDEX audit_log_wiki_idx ON audit_log (wiki_id, created_at DESC);
CREATE INDEX audit_log_user_idx ON audit_log (user_id, created_at DESC);

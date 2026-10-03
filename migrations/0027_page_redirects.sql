-- Renaming a page leaves its old address leading to the new one. A page has
-- a row per language under one slug, so a redirect goes from an address to
-- an address, not to one row. The first schema's `redirects` table pointed
-- at a single page and nothing ever wrote it; it goes, but only while empty.

DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM redirects) THEN
    RAISE EXCEPTION 'redirects is not empty; move its rows to page_redirects by hand first';
  END IF;
END $$;
DROP TABLE redirects;

CREATE TABLE page_redirects (
  wiki_id      UUID NOT NULL REFERENCES wikis (id) ON DELETE CASCADE,
  namespace    page_namespace NOT NULL,
  from_slug    TEXT NOT NULL,
  to_namespace page_namespace NOT NULL,
  to_slug      TEXT NOT NULL,
  created_by   UUID NULL REFERENCES users (id) ON DELETE SET NULL,
  created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (wiki_id, namespace, from_slug)
);
CREATE INDEX page_redirects_target_idx ON page_redirects (wiki_id, to_namespace, to_slug);

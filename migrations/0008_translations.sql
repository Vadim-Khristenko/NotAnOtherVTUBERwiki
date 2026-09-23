-- NotAnotherWiki Engine, articles in several languages.
--
-- One article, one slug, one page row per language (pages.locale). A page that
-- was translated from another language remembers which one, and which revision
-- of it the translation matches. When the source moves on, readers of the
-- translation are told it may be out of date.

ALTER TABLE pages ADD COLUMN translation_source_locale TEXT NULL;
ALTER TABLE pages ADD COLUMN translation_source_revision_id UUID NULL
  REFERENCES revisions(id) ON DELETE SET NULL;

-- The interlanguage list on every article looks up the other languages of a
-- slug; this keeps that an index scan.
CREATE INDEX pages_slug_locales_idx ON pages (wiki_id, namespace, slug) WHERE deleted_at IS NULL;

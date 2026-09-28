-- Drafts kept on the server while somebody writes, so a closed tab, another
-- device or a crash does not cost the text. Private to their author.
--
-- An edit draft belongs to one page in one language: at most one per author.
-- A new-page draft has no page yet, so each has its own id and an author may
-- keep several; `path` is whatever address they typed so far.

CREATE TABLE drafts (
  id                UUID PRIMARY KEY,
  wiki_id           UUID NOT NULL REFERENCES wikis(id) ON DELETE CASCADE,
  user_id           UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  kind              TEXT NOT NULL CHECK (kind IN ('new', 'edit')),
  -- The page path for an edit; the typed address, maybe empty, for a new page.
  path              TEXT NOT NULL DEFAULT '',
  locale            TEXT NOT NULL,
  title             TEXT NOT NULL DEFAULT '',
  summary           TEXT NOT NULL DEFAULT '',
  body_md           TEXT NOT NULL DEFAULT '',
  -- The revision an edit started from, so saving it later still meets the
  -- conflict check instead of writing over somebody else's edit.
  base_revision_id  UUID NULL REFERENCES revisions(id) ON DELETE SET NULL,
  created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX drafts_edit_once_idx ON drafts (user_id, wiki_id, locale, path)
  WHERE kind = 'edit';
CREATE INDEX drafts_mine_idx ON drafts (user_id, wiki_id, updated_at DESC);

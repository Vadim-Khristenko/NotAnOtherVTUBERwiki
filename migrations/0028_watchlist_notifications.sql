-- A watchlist and notifications. A reader watches a page by its address, so
-- a watch covers every language version and follows a rename. Notifications
-- tell a person what concerns them: an edit to a page they watch, the
-- review of their own edit, the answer to their report.

CREATE TABLE watches (
  user_id    UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  wiki_id    UUID NOT NULL REFERENCES wikis (id) ON DELETE CASCADE,
  namespace  page_namespace NOT NULL,
  slug       TEXT NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (user_id, wiki_id, namespace, slug)
);
-- Who watches a page, asked on every save.
CREATE INDEX watches_page_idx ON watches (wiki_id, namespace, slug);

CREATE TABLE notifications (
  id         UUID PRIMARY KEY,
  user_id    UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  wiki_id    UUID NOT NULL REFERENCES wikis (id) ON DELETE CASCADE,
  kind       TEXT NOT NULL
    CHECK (kind IN ('page_edited', 'edit_accepted', 'edit_rejected', 'report_answered')),
  actor_id   UUID NULL REFERENCES users (id) ON DELETE SET NULL,
  -- The page title or the report's subject, as it read then.
  title      TEXT NOT NULL,
  -- A local path to open: the page, its diff, the report.
  link       TEXT NOT NULL,
  -- The edit summary, the reviewer's note or the answer, when there is one.
  note       TEXT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  read_at    TIMESTAMPTZ NULL
);
-- The header counts unread ones on every page a signed-in reader opens.
CREATE INDEX notifications_unread_idx ON notifications (user_id, wiki_id) WHERE read_at IS NULL;
CREATE INDEX notifications_list_idx ON notifications (user_id, wiki_id, created_at DESC);

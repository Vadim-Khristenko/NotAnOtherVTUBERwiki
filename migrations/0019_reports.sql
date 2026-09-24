-- Reports from readers to the wiki's staff: a mistake in an article, a
-- complaint about a page or a person, or a change to a page the reader may
-- not edit. Moderators work them from a queue. The subject is kept as text
-- too, so a report outlives the page it was about.
CREATE TABLE reports (
  id           UUID PRIMARY KEY,
  wiki_id      UUID NOT NULL REFERENCES wikis(id) ON DELETE CASCADE,
  kind         TEXT NOT NULL CHECK (kind IN ('mistake', 'complaint', 'suggestion')),
  -- For a complaint: spam, vandalism, abuse, copyright, privacy, other.
  reason       TEXT NULL,
  -- A page path (`lore`, `template:x`) or `user:name`.
  subject      TEXT NOT NULL,
  page_id      UUID NULL REFERENCES pages(id) ON DELETE SET NULL,
  locale       TEXT NULL,
  -- The revision the reader saw, so a suggestion is compared with what it
  -- was written against.
  revision_id  UUID NULL REFERENCES revisions(id) ON DELETE SET NULL,
  message      TEXT NOT NULL,
  -- A suggestion's whole proposed text.
  proposed     TEXT NULL,
  reporter_id  UUID NULL REFERENCES users(id) ON DELETE SET NULL,
  status       TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'resolved', 'dismissed')),
  handled_by   UUID NULL REFERENCES users(id) ON DELETE SET NULL,
  handled_at   TIMESTAMPTZ NULL,
  -- The staff member's answer, which the reporter may read.
  response     TEXT NULL,
  created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX reports_queue_idx ON reports (wiki_id, status, created_at DESC);
CREATE INDEX reports_reporter_idx ON reports (reporter_id, created_at DESC);

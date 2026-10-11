-- Discussions: topics about a page, or about one change to it, with replies.
-- A topic belongs to the page's address, in every language, and follows the
-- page when it moves. One about a change keeps the revision and, when it was
-- about part of the change, which run of changes in the diff and a quote.

CREATE TABLE threads (
  id               UUID PRIMARY KEY,
  wiki_id          UUID NOT NULL REFERENCES wikis (id) ON DELETE CASCADE,
  namespace        page_namespace NOT NULL DEFAULT 'main',
  slug             TEXT NOT NULL,
  -- About one change: the revision it made. NULL for the page as a whole.
  revision_id      UUID NULL REFERENCES revisions (id) ON DELETE SET NULL,
  -- The run of changes in that revision's diff, numbered from 1.
  hunk             INT NULL CHECK (hunk IS NULL OR hunk > 0),
  -- The words of the change the topic is about.
  quote            TEXT NULL,
  title            TEXT NOT NULL,
  author_id        UUID NULL REFERENCES users (id) ON DELETE SET NULL,
  status           TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'resolved')),
  resolved_by      UUID NULL REFERENCES users (id) ON DELETE SET NULL,
  locked           BOOLEAN NOT NULL DEFAULT false,
  hidden_at        TIMESTAMPTZ NULL,
  hidden_by        UUID NULL REFERENCES users (id) ON DELETE SET NULL,
  replies          INT NOT NULL DEFAULT 0,
  created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
  last_post_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
  title_vector     tsvector GENERATED ALWAYS AS (to_tsvector('simple', title)) STORED
);

CREATE INDEX threads_page_idx ON threads (wiki_id, namespace, slug, last_post_at DESC);
CREATE INDEX threads_recent_idx ON threads (wiki_id, last_post_at DESC);
CREATE INDEX threads_revision_idx ON threads (revision_id) WHERE revision_id IS NOT NULL;
CREATE INDEX threads_title_search_idx ON threads USING gin (title_vector);

CREATE TABLE posts (
  id           UUID PRIMARY KEY,
  thread_id    UUID NOT NULL REFERENCES threads (id) ON DELETE CASCADE,
  author_id    UUID NULL REFERENCES users (id) ON DELETE SET NULL,
  -- The message this one answers, shown as a quote above it.
  reply_to     UUID NULL REFERENCES posts (id) ON DELETE SET NULL,
  body_md      TEXT NOT NULL,
  created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
  edited_at    TIMESTAMPTZ NULL,
  -- Taken down by a moderator, or by its author.
  hidden_at    TIMESTAMPTZ NULL,
  hidden_by    UUID NULL REFERENCES users (id) ON DELETE SET NULL,
  body_vector  tsvector GENERATED ALWAYS AS (to_tsvector('simple', body_md)) STORED
);

CREATE INDEX posts_thread_idx ON posts (thread_id, created_at);
CREATE INDEX posts_author_idx ON posts (author_id, created_at DESC);
CREATE INDEX posts_search_idx ON posts USING gin (body_vector);

-- Search over the article as a reader sees it, one row per piece.
--
-- A piece is a stretch of a section's plain text, a few thousand characters
-- long, with the section's heading and anchor so a result links to it. The
-- hash covers the text and its stemming, so a save rewrites only the pieces
-- that changed and keeps every other vector. pages.search_vector now holds the
-- title and summary only; the body is all here, short pages included.
--
-- The old table held raw Markdown in 200k character pieces and only for long
-- pages; it goes. Run `naw reindex` after this migration to fill the new one.

DROP TABLE IF EXISTS page_search_chunks;

CREATE TABLE search_chunks (
  page_id   UUID NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
  -- Repeated from the page so a query filters on this table's own index.
  wiki_id   UUID NOT NULL REFERENCES wikis (id) ON DELETE CASCADE,
  -- From 1, in reading order.
  chunk_no  INT NOT NULL,
  -- The heading's id on the page, empty before the first heading.
  anchor    TEXT NOT NULL DEFAULT '',
  heading   TEXT NOT NULL DEFAULT '',
  body      TEXT NOT NULL,
  text_hash BYTEA NOT NULL,
  vector    tsvector NOT NULL,
  PRIMARY KEY (page_id, chunk_no)
);

CREATE INDEX search_chunks_vector_idx ON search_chunks USING gin (vector);
CREATE INDEX search_chunks_wiki_idx ON search_chunks (wiki_id);

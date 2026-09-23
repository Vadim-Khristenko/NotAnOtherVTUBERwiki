-- NotAnotherWiki Engine, full text search over the whole of a long article.
--
-- A tsvector may not pass 1 MB, and an article may be 5 MB. pages.search_vector
-- keeps the title, the summary and the first part of the body, which is all of
-- it for nearly every page. A longer body is cut into pieces, and each piece
-- gets its own vector here, so every word of the article is found and no single
-- vector comes near the limit. Short pages have no rows in this table at all.

CREATE TABLE page_search_chunks (
  page_id    UUID NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
  -- 1 for the piece right after the head in pages.search_vector, and so on.
  chunk_no   INT NOT NULL,
  -- Where the piece sits in the body, in characters, for the search snippet.
  start_char INT NOT NULL,
  len_chars  INT NOT NULL,
  vector     tsvector NOT NULL,
  PRIMARY KEY (page_id, chunk_no)
);

CREATE INDEX page_search_chunks_vector_idx ON page_search_chunks USING gin (vector);

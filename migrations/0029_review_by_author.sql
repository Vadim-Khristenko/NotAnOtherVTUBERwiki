-- An author's own edits that wait for review or were turned down, listed on
-- their drafts page. Accepted revisions, nearly all of them, stay out.
CREATE INDEX revisions_author_review_idx ON revisions (author_id, created_at DESC)
  WHERE review_status <> 'accepted';

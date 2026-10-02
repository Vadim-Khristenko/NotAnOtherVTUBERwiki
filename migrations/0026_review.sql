-- Edits that wait for review, as with MediaWiki's pending changes. Where a
-- wiki turns it on, a new page or an edit by someone without the pass is
-- stored as a pending revision: readers keep the last accepted text, and a
-- curator accepts or rejects it from the review queue.

ALTER TABLE revisions ADD COLUMN review_status TEXT NOT NULL DEFAULT 'accepted'
  CHECK (review_status IN ('accepted', 'pending', 'rejected'));
-- The revision a pending edit was written over; accepting it is refused
-- once the page has moved on, so nobody's later edit is lost.
ALTER TABLE revisions ADD COLUMN base_revision_id UUID NULL REFERENCES revisions (id) ON DELETE SET NULL;
-- The title a pending edit proposes; the page keeps its own until then.
ALTER TABLE revisions ADD COLUMN review_title TEXT NULL;
ALTER TABLE revisions ADD COLUMN reviewed_by UUID NULL REFERENCES users (id) ON DELETE SET NULL;
ALTER TABLE revisions ADD COLUMN reviewed_at TIMESTAMPTZ NULL;
ALTER TABLE revisions ADD COLUMN review_note TEXT NULL;

CREATE INDEX revisions_pending_idx ON revisions (page_id, created_at) WHERE review_status = 'pending';

-- The size of each revision, kept beside it. The history shows how much every
-- edit added or removed, and computing that from the bodies would read every
-- body of the page on each view.
ALTER TABLE revisions
  ADD COLUMN bytes integer GENERATED ALWAYS AS (octet_length(body_md)) STORED;

-- More kinds of notification. Two are new: edits that wait for the reviewer
-- who reads it (one unread entry per reviewer, refreshed while the queue
-- fills), and an author who earned the pass by having enough edits accepted.
--
-- The discussion kinds were sent since 0039 but never allowed here, so the
-- check turned every one of them away; they are allowed now.

ALTER TABLE notifications DROP CONSTRAINT notifications_kind_check;
ALTER TABLE notifications ADD CONSTRAINT notifications_kind_check
  CHECK (kind IN ('page_edited', 'edit_accepted', 'edit_rejected', 'report_answered',
                  'edit_waiting', 'edit_trusted',
                  'discussion_new', 'discussion_change', 'discussion_reply',
                  'discussion_mention', 'discussion_resolved'));

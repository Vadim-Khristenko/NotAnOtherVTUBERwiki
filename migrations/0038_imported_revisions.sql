-- Revisions brought over from another wiki (`naw import`): the name their
-- author had there and the wiki's name, since that person has no account
-- here. author_id stays NULL for them.
ALTER TABLE revisions ADD COLUMN imported_author TEXT NULL;
ALTER TABLE revisions ADD COLUMN imported_from TEXT NULL;

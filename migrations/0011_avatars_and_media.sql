-- NotAnotherWiki Engine, avatars and uploaded images.
--
-- Avatars belong to the account, not to a wiki, so they are a storage key on
-- users rather than a row in media. Uploaded images for articles use the media
-- table from 0001, one row per wiki per file; the storage key is the content
-- hash, so the same file uploaded twice is stored once.

ALTER TABLE users ADD COLUMN avatar_key TEXT NULL;

CREATE UNIQUE INDEX media_wiki_key_uq ON media (wiki_id, storage_key);
CREATE INDEX media_uploader_idx ON media (wiki_id, uploader_id, created_at DESC);

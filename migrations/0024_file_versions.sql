-- A file keeps every version uploaded under its name, as on Wikimedia
-- Commons: a better quality or another format of the same picture replaces
-- it everywhere it is used, and an older version can be brought back. The
-- `media` row stays the file as it is now; `media_versions` is its history,
-- the current version included.

CREATE TABLE media_versions (
  id          UUID PRIMARY KEY,
  media_id    UUID NOT NULL REFERENCES media (id) ON DELETE CASCADE,
  storage_key TEXT NOT NULL,
  filename    TEXT NOT NULL,
  mime        TEXT NOT NULL,
  size_bytes  BIGINT NOT NULL DEFAULT 0,
  width       INT NULL,
  height      INT NULL,
  uploader_id UUID NULL REFERENCES users (id) ON DELETE SET NULL,
  comment     TEXT NULL,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX media_versions_media_idx ON media_versions (media_id, created_at DESC);
-- Serving a file and removing a stored one both ask who still points at a key.
CREATE INDEX media_versions_key_idx ON media_versions (storage_key);
-- The upload allowance counts versions too.
CREATE INDEX media_versions_uploader_idx ON media_versions (uploader_id, created_at DESC);

INSERT INTO media_versions (id, media_id, storage_key, filename, mime, size_bytes, width, height,
                            uploader_id, created_at)
SELECT gen_random_uuid(), id, storage_key, filename, mime, size_bytes, width, height,
       uploader_id, created_at
FROM media;

-- A moderator may hide a file: readers get neither the file nor its page,
-- and articles show a link in its place.
ALTER TABLE media ADD COLUMN hidden_at TIMESTAMPTZ NULL;
ALTER TABLE media ADD COLUMN hidden_by UUID NULL REFERENCES users (id) ON DELETE SET NULL;
ALTER TABLE media ADD COLUMN hidden_reason TEXT NULL;

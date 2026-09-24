-- Every uploaded file gets a page, as on Wikimedia Commons: /image:ferris.png,
-- /audio:theme.ogg, /video:clip.webm, /file:notes.pdf. `name` is the part
-- after the prefix, unique per wiki; `kind` picks the prefix and the player.
-- A description, with its own history, is an ordinary page in the `file`
-- namespace under the same name.

ALTER TABLE media ADD COLUMN name TEXT;
ALTER TABLE media ADD COLUMN kind TEXT NOT NULL DEFAULT 'image'
  CHECK (kind IN ('image', 'audio', 'video', 'document'));

-- Names for the files already here: the upload's file name reduced to
-- lowercase letters, digits and dashes, with its type's extension. A second
-- file of the same name takes a piece of its id, which is unique.
WITH base AS (
  SELECT id, wiki_id, created_at,
         left(trim(both '-' from regexp_replace(
           lower(regexp_replace(filename, '\.[^.]*$', '')), '[^a-z0-9]+', '-', 'g')), 80) AS stem,
         CASE split_part(mime, '/', 2) WHEN 'jpeg' THEN 'jpg' ELSE split_part(mime, '/', 2) END AS ext
  FROM media
), numbered AS (
  SELECT id, CASE WHEN stem = '' THEN 'file' ELSE stem END AS stem, ext,
         row_number() OVER (PARTITION BY wiki_id, CASE WHEN stem = '' THEN 'file' ELSE stem END, ext
                            ORDER BY created_at, id) AS n
  FROM base
)
UPDATE media m
SET name = n.stem || CASE WHEN n.n > 1 THEN '-' || substr(m.id::text, 1, 8) ELSE '' END || '.' || n.ext
FROM numbered n
WHERE n.id = m.id;

ALTER TABLE media ALTER COLUMN name SET NOT NULL;
CREATE UNIQUE INDEX media_wiki_name_uq ON media (wiki_id, name);

-- Which files each page shows, rewritten on every save, for "used on".
CREATE TABLE file_uses (
  page_id     UUID NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
  wiki_id     UUID NOT NULL REFERENCES wikis (id) ON DELETE CASCADE,
  storage_key TEXT NOT NULL,
  PRIMARY KEY (page_id, storage_key)
);
CREATE INDEX file_uses_key_idx ON file_uses (wiki_id, storage_key);

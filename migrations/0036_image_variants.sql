-- Smaller WebP copies of uploaded pictures, drawn by the worker. One row per
-- picture and width; the file itself lives in storage under thumbs/. Until a
-- copy is ready, its address redirects to the original, so a reader never
-- waits on the worker.
CREATE TABLE image_variants (
  -- The original's storage key, media/xx/<sha256>.<ext>.
  source_key  TEXT NOT NULL,
  width       INTEGER NOT NULL CHECK (width IN (480, 960, 1600)),
  -- pending: waits for the worker; rendering: claimed; ready: stored;
  -- failed: the worker could not make it, error says why.
  status      TEXT NOT NULL DEFAULT 'pending'
              CHECK (status IN ('pending', 'rendering', 'ready', 'failed')),
  bytes       INTEGER NULL,
  error       TEXT NULL,
  attempts    INTEGER NOT NULL DEFAULT 0,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  claimed_at  TIMESTAMPTZ NULL,
  rendered_at TIMESTAMPTZ NULL,
  PRIMARY KEY (source_key, width)
);

CREATE INDEX image_variants_pending ON image_variants (created_at) WHERE status = 'pending';

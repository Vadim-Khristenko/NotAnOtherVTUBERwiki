-- Diagrams drawn by the worker: one row per diagram and theme, addressed by
-- the hash of the fence's language and text. Content addressed, so the same
-- diagram on two pages or two wikis is drawn once. The page keeps showing the
-- source until the drawing is ready, and a reader never waits on the worker.
CREATE TABLE diagrams (
  -- sha256 hex of the drawing version, the language and the source.
  hash        TEXT NOT NULL,
  -- 'light' or 'dark': a drawing is an image, so it cannot follow the page's theme.
  theme       TEXT NOT NULL CHECK (theme IN ('light', 'dark')),
  lang        TEXT NOT NULL CHECK (lang IN ('mermaid', 'dot')),
  source      TEXT NOT NULL,
  -- pending: waits for the worker; rendering: claimed; ready: svg is set;
  -- failed: the source cannot be drawn, error says why.
  status      TEXT NOT NULL DEFAULT 'pending'
              CHECK (status IN ('pending', 'rendering', 'ready', 'failed')),
  -- Sanitized by the engine before it is stored.
  svg         TEXT NULL,
  width       INTEGER NULL,
  height      INTEGER NULL,
  error       TEXT NULL,
  attempts    INTEGER NOT NULL DEFAULT 0,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  claimed_at  TIMESTAMPTZ NULL,
  rendered_at TIMESTAMPTZ NULL,
  PRIMARY KEY (hash, theme)
);

-- The runner takes the oldest pending drawing.
CREATE INDEX diagrams_pending ON diagrams (created_at) WHERE status = 'pending';

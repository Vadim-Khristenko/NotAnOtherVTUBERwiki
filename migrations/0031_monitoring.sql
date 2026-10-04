-- Monitoring for the admins: what the server did, minute by minute, and
-- the errors it hit. The engine keeps the counts in memory and writes one
-- row per minute and route group; percentiles come from the histogram, so
-- minutes add up into hours and days without losing them.

CREATE TABLE metrics_minutes (
  minute        TIMESTAMPTZ NOT NULL,
  route         TEXT NOT NULL,
  requests      INT NOT NULL,
  server_errors INT NOT NULL,
  client_errors INT NOT NULL,
  -- Requests per latency bucket; the bounds live in the engine (metrics.rs).
  hist          INT[] NOT NULL,
  sum_ms        BIGINT NOT NULL,
  max_ms        INT NOT NULL,
  PRIMARY KEY (minute, route)
);

-- The most requests in flight at once during each minute.
CREATE TABLE metrics_load (
  minute         TIMESTAMPTZ PRIMARY KEY,
  peak_in_flight INT NOT NULL
);

-- An error: a 5xx answer, or anything the engine logged at error level.
-- Same fingerprint, same problem, so the page can group them.
CREATE TABLE error_events (
  id          UUID PRIMARY KEY,
  at          TIMESTAMPTZ NOT NULL DEFAULT now(),
  source      TEXT NOT NULL CHECK (source IN ('response', 'log')),
  fingerprint TEXT NOT NULL,
  message     TEXT NOT NULL,
  request_id  TEXT NULL,
  method      TEXT NULL,
  path        TEXT NULL,
  status      INT NULL
);
CREATE INDEX error_events_at_idx ON error_events (at DESC);
CREATE INDEX error_events_fingerprint_idx ON error_events (fingerprint, at DESC);

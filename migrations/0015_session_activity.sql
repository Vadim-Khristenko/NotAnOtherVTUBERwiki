-- When a session was last used, so a count of signed-in devices can leave out
-- the browser that was closed a month ago and never came back.
ALTER TABLE sessions ADD COLUMN last_seen_at timestamptz NOT NULL DEFAULT now();
UPDATE sessions SET last_seen_at = created_at;
CREATE INDEX sessions_user_seen_idx ON sessions (user_id, last_seen_at DESC);

-- The last sign-in lives with the account. Read from the sessions table, it
-- vanished the moment the person signed out.
ALTER TABLE users ADD COLUMN last_sign_in_at timestamptz NULL;
UPDATE users u SET last_sign_in_at = s.at
  FROM (SELECT user_id, max(created_at) AS at FROM sessions GROUP BY user_id) s
  WHERE s.user_id = u.id;

DELETE FROM sessions WHERE expires_at <= now();

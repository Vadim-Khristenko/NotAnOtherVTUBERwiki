-- The wiki's Telegram bot: a person links their chat from their settings,
-- and the bot can then send a password reset link, sign-in alerts and their
-- notifications. A chat belongs to one account at a time.
CREATE TABLE telegram_links (
  user_id     UUID PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
  chat_id     BIGINT NOT NULL UNIQUE,
  tg_username TEXT NULL,
  -- Telegram's language for the person, for when the account follows the
  -- browser and so names none.
  language    TEXT NULL,
  linked_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- A password reset link. Only the SHA-256 of its token is kept; it works
-- once, for a short while, and a newer one or a used one ends the rest.
CREATE TABLE password_resets (
  id          UUID PRIMARY KEY,
  user_id     UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  token_hash  BYTEA NOT NULL UNIQUE,
  channel     TEXT NOT NULL CHECK (channel IN ('telegram', 'email')),
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  expires_at  TIMESTAMPTZ NOT NULL,
  used_at     TIMESTAMPTZ NULL
);
CREATE INDEX password_resets_user_idx ON password_resets (user_id);

-- When a notification went out to Telegram, so it goes once, and an entry
-- refreshed by a later edit does not go again.
ALTER TABLE notifications ADD COLUMN forwarded_at TIMESTAMPTZ NULL;
CREATE INDEX notifications_forward_idx ON notifications (created_at)
  WHERE forwarded_at IS NULL AND read_at IS NULL;

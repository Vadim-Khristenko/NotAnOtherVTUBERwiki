-- The Discord bot beside the Telegram one: a Discord account linked to a
-- wiki account, and the language a chat chose with /lang on either bot.

CREATE TABLE discord_links (
  user_id    UUID PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
  discord_id TEXT NOT NULL UNIQUE,
  username   TEXT NULL,
  language   TEXT NULL,
  linked_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- What a chat asked to be talked to in, linked or not. It wins over the
-- account's language and the app's.
CREATE TABLE bot_chat_languages (
  platform  TEXT NOT NULL CHECK (platform IN ('telegram', 'discord')),
  chat_id   TEXT NOT NULL,
  language  TEXT NOT NULL,
  chosen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (platform, chat_id)
);

-- A reset link may now go out through Discord too.
ALTER TABLE password_resets DROP CONSTRAINT password_resets_channel_check;
ALTER TABLE password_resets ADD CONSTRAINT password_resets_channel_check
  CHECK (channel IN ('telegram', 'discord', 'email'));

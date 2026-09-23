-- NotAnotherWiki Engine, 7TV emotes kept on this server.
--
-- A source is one 7TV user (their active emote set) or one emote set, added
-- by an admin. The sources are the allowlist: only their emotes exist here.
-- Each emote file is downloaded once into storage, so a reader's browser never
-- asks 7TV for anything, and the article keeps its pictures if 7TV changes.

CREATE TABLE emote_sources (
  id          UUID PRIMARY KEY,
  wiki_id     UUID NOT NULL REFERENCES wikis (id) ON DELETE CASCADE,
  provider    TEXT NOT NULL DEFAULT '7tv',
  kind        TEXT NOT NULL CHECK (kind IN ('user', 'set')),
  -- The 7TV user or emote set id.
  ref         TEXT NOT NULL,
  -- Who or what it is, for the admin list: a 7TV username or a set name.
  label       TEXT NOT NULL DEFAULT '',
  -- The set the last sync read, which for a user is their active one.
  set_id      TEXT NULL,
  status      TEXT NOT NULL DEFAULT 'pending'
              CHECK (status IN ('pending', 'syncing', 'ok', 'error')),
  error       TEXT NULL,
  emote_count INT NOT NULL DEFAULT 0,
  skipped     INT NOT NULL DEFAULT 0,
  started_at  TIMESTAMPTZ NULL,
  synced_at   TIMESTAMPTZ NULL,
  added_by    UUID NULL REFERENCES users (id) ON DELETE SET NULL,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE (wiki_id, provider, kind, ref)
);

CREATE TABLE emotes (
  wiki_id     UUID NOT NULL REFERENCES wikis (id) ON DELETE CASCADE,
  -- As written in an article between colons. Case matters, as on 7TV.
  name        TEXT NOT NULL,
  source_id   UUID NOT NULL REFERENCES emote_sources (id) ON DELETE CASCADE,
  provider_id TEXT NOT NULL,
  storage_key TEXT NOT NULL,
  width       INT NOT NULL,
  height      INT NOT NULL,
  animated    BOOLEAN NOT NULL DEFAULT false,
  size_bytes  BIGINT NOT NULL,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (wiki_id, name)
);

CREATE INDEX emotes_source_idx ON emotes (source_id);

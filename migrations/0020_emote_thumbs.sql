-- A small copy of each emote (7TV's 1x) for the lists: the emotes page and
-- the editor's picker show hundreds at once, and the 2x file an article
-- needs is two to three times heavier. Filled on the next sync.
ALTER TABLE emotes
  ADD COLUMN thumb_key   TEXT NULL,
  ADD COLUMN thumb_bytes BIGINT NOT NULL DEFAULT 0;

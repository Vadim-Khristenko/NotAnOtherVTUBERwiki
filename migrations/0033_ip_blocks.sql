-- Address blocks, as on Wikipedia: a blocked address may read the wiki but
-- not write to it. Moderators and up are not held by them, so a moderator on a
-- shared address keeps working; curators and below are.
CREATE TABLE ip_blocks (
  id         UUID PRIMARY KEY,
  wiki_id    UUID NOT NULL REFERENCES wikis(id) ON DELETE CASCADE,
  -- One address (a /32 or /128) or a range.
  network    CIDR NOT NULL,
  -- Shown to the admins only, never to the blocked reader.
  reason     TEXT NOT NULL DEFAULT '',
  created_by UUID NULL REFERENCES users(id) ON DELETE SET NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  -- NULL for a block without an end.
  expires_at TIMESTAMPTZ NULL,
  UNIQUE (wiki_id, network)
);

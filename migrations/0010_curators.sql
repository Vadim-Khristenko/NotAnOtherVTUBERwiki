-- NotAnotherWiki Engine, curators and page protection levels.
--
-- A curator is a trusted editor who looks after other people: between sponsor
-- and moderator. Curators and up can protect a page at a level, meaning only
-- that role and above may edit it. The old boolean lock meant "moderators and
-- up", so every locked page becomes exactly that.

ALTER TYPE user_wiki_role ADD VALUE IF NOT EXISTS 'curator' BEFORE 'moderator';

ALTER TABLE pages ADD COLUMN edit_level TEXT NULL
  CHECK (edit_level IN ('curator', 'moderator', 'admin', 'owner'));
UPDATE pages SET edit_level = 'moderator' WHERE is_locked;

-- Who looks after whom on a wiki. A person has at most one curator per wiki;
-- that curator may edit the person's profile alongside them.
CREATE TABLE curatorships (
  wiki_id      UUID NOT NULL REFERENCES wikis(id) ON DELETE CASCADE,
  user_id      UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  curator_id   UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  assigned_by  UUID NULL REFERENCES users(id) ON DELETE SET NULL,
  assigned_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (wiki_id, user_id),
  CHECK (user_id <> curator_id)
);
CREATE INDEX curatorships_curator_idx ON curatorships (wiki_id, curator_id);

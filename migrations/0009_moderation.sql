-- NotAnotherWiki Engine, moderating accounts.
--
-- Three things an admin records about a person, on top of their role:
-- individual permission overrides, sanctions, and notes only admins read.

-- A capability granted or denied to one account on one wiki, over what their
-- role gives. A deny wins over the role; an allow adds to it.
CREATE TABLE user_capabilities (
  user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  wiki_id     UUID NOT NULL REFERENCES wikis(id) ON DELETE CASCADE,
  capability  TEXT NOT NULL,            -- validated in code, see perm::Capability
  allowed     BOOLEAN NOT NULL,
  set_by      UUID NULL REFERENCES users(id) ON DELETE SET NULL,
  set_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (user_id, wiki_id, capability)
);

-- Mutes and bans. wiki_id NULL is install-wide, and only a ban can be
-- install-wide: the account cannot sign in anywhere. A sanction is active
-- until it expires or is lifted; lifted ones stay as the person's history.
CREATE TABLE sanctions (
  id           UUID PRIMARY KEY,
  user_id      UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  wiki_id      UUID NULL REFERENCES wikis(id) ON DELETE CASCADE,
  kind         TEXT NOT NULL CHECK (kind IN ('mute', 'ban')),
  reason       TEXT NOT NULL,
  created_by   UUID NULL REFERENCES users(id) ON DELETE SET NULL,
  created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
  expires_at   TIMESTAMPTZ NULL,
  lifted_at    TIMESTAMPTZ NULL,
  lifted_by    UUID NULL REFERENCES users(id) ON DELETE SET NULL,
  lift_reason  TEXT NULL,
  CHECK (wiki_id IS NOT NULL OR kind = 'ban')
);
CREATE INDEX sanctions_active_idx ON sanctions (user_id) WHERE lifted_at IS NULL;

-- Notes about a person, for the admins of one wiki and nobody else.
CREATE TABLE user_notes (
  id          UUID PRIMARY KEY,
  user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  wiki_id     UUID NOT NULL REFERENCES wikis(id) ON DELETE CASCADE,
  author_id   UUID NULL REFERENCES users(id) ON DELETE SET NULL,
  body        TEXT NOT NULL,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX user_notes_user_idx ON user_notes (user_id, wiki_id, created_at DESC);

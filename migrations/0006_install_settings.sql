-- NotAnotherWiki Engine, settings that belong to the install, not to a wiki.
--
-- Accounts are shared by every wiki on an install, so rules about accounts
-- (may people rename themselves, how long an old name stays reserved) cannot
-- live in wikis.settings. One row per group of settings, JSON inside, so a
-- new setting needs no migration. Anything absent falls back to the value
-- from the environment.

CREATE TABLE install_settings (
  key         TEXT PRIMARY KEY,
  value       JSONB NOT NULL DEFAULT '{}',
  updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_by  UUID NULL REFERENCES users(id) ON DELETE SET NULL
);

-- When the username last changed. The cooldown needs it, and the alias rows
-- cannot carry it: the oldest ones are pruned when an account hits its limit.
ALTER TABLE users ADD COLUMN username_changed_at TIMESTAMPTZ NULL;

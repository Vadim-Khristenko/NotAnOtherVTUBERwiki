-- The wiki's documents (terms and privacy) and what each person agreed to.
--
-- The current version lives in wikis.settings.legal, bumped by an admin when a
-- document changes in substance. A person's row says which version they last
-- accepted, at sign-up or after a change, so the wiki knows whom to tell.
ALTER TABLE users ADD COLUMN policy_version INT NOT NULL DEFAULT 0;
ALTER TABLE users ADD COLUMN policy_accepted_at TIMESTAMPTZ NULL;

-- A deleted account keeps its row, so the edits it made keep an author, but
-- loses its name, profile, contacts and every way to sign in.
ALTER TABLE users ADD COLUMN deleted_at TIMESTAMPTZ NULL;

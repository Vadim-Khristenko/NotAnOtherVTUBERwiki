-- NotAnotherWiki Engine, local passwords for invite-only wikis.
--
-- During a closed alpha nobody signs themselves up: an admin creates the
-- account with a temporary password, and the owner has to replace it on the
-- first sign-in. `password_hash` already exists from 0001 (Argon2id PHC).

-- Set on every admin-issued password. While true, a session can reach only the
-- change password page and sign out.
ALTER TABLE users ADD COLUMN must_change_password BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE users ADD COLUMN password_changed_at TIMESTAMPTZ NULL;

-- Who created the account, when it was not the person themselves. Kept when
-- that admin is deleted, because the account outlives the invitation.
ALTER TABLE users ADD COLUMN created_by UUID NULL REFERENCES users(id) ON DELETE SET NULL;

-- Sign-in compares lowercase names; usernames are lowercase by rule already,
-- this makes the lookup an index scan and not a sequential one.
CREATE INDEX users_username_lower_idx ON users (lower(username));

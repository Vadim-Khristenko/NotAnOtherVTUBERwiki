-- Two-step sign-in with a time-based code (TOTP, RFC 6238), the six digits an
-- authenticator app shows, on the tables 0002 laid out: `totp_credentials`
-- holds the key, encrypted under the server's `auth.secret_key`, and
-- `recovery_codes` the spares for a lost phone.
--
-- A session started by someone who turned it on waits for the code: until
-- then the session layer lets it reach only the code page and the way out,
-- whatever way in was used.

-- The last 30-second step a code was taken for, so each code works once.
ALTER TABLE totp_credentials ADD COLUMN last_step BIGINT NULL;

-- Recovery codes carry about 50 random bits, so they are kept as SHA-256
-- digests rather than with argon2 as 0002 planned: the sign-in throttle
-- limits the tries, and a slow hash would cost a second for every try against
-- ten codes. One account never holds the same code twice.
CREATE UNIQUE INDEX recovery_codes_user_code_idx ON recovery_codes (user_id, code_hash);

ALTER TABLE sessions ADD COLUMN second_factor_pending BOOLEAN NOT NULL DEFAULT false;

-- The other way to the second step: a one-time code the wiki's bot sends to
-- the Telegram or Discord chat the account linked. It counts only while a
-- chat is linked and the server runs a bot, so it never locks anybody out.
ALTER TABLE users ADD COLUMN second_factor_bot BOOLEAN NOT NULL DEFAULT false;

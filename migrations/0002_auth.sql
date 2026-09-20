-- NotAnotherWiki Engine, auth schema (guide sections 3 and 22).
-- One migration for the whole auth slice: nullable credentials, identity
-- links, email verification, plus the 2FA, trusted device and notification
-- tables. Every token stored here is a hash, never the raw value.

-- OAuth users arrive without a password; Steam arrives without an email.
ALTER TABLE users ALTER COLUMN password_hash DROP NOT NULL;
ALTER TABLE users ALTER COLUMN email DROP NOT NULL;

-- Preferences, privacy switches and the notification matrix live here.
ALTER TABLE users ADD COLUMN settings JSONB NOT NULL DEFAULT '{}';

-- Case-insensitive email uniqueness on top of the plain UNIQUE from 0001.
-- The application normalizes to lowercase before writing; this is a belt.
CREATE UNIQUE INDEX users_email_lower_uq
  ON users (lower(email)) WHERE email IS NOT NULL;

CREATE TABLE oauth_identities (
  id               UUID PRIMARY KEY,
  user_id          UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  provider         TEXT NOT NULL,           -- validated in code, no CHECK
  provider_user_id TEXT NOT NULL,
  email            TEXT NULL,               -- only when the provider certified it
  display_name     TEXT NULL,
  avatar_url       TEXT NULL,
  raw              JSONB NOT NULL DEFAULT '{}',  -- trimmed profile, never tokens
  created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
  last_login_at    TIMESTAMPTZ NULL,
  UNIQUE (provider, provider_user_id)
);

CREATE INDEX oauth_identities_user_idx ON oauth_identities (user_id);

-- Per-identity messaging consent (Telegram DM, Discord user install).
ALTER TABLE oauth_identities ADD COLUMN dm_consent BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE oauth_identities ADD COLUMN dm_consent_at TIMESTAMPTZ NULL;

CREATE TABLE email_verifications (
  id          UUID PRIMARY KEY,
  user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  email       TEXT NOT NULL,                -- the address being verified
  token_hash  BYTEA NOT NULL,               -- sha256 of the emailed token
  expires_at  TIMESTAMPTZ NOT NULL,
  consumed_at TIMESTAMPTZ NULL,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX email_verifications_user_idx ON email_verifications (user_id);
CREATE UNIQUE INDEX email_verifications_token_uq ON email_verifications (token_hash);

-- Session lookups by user for logout everywhere.
CREATE INDEX sessions_user_idx ON sessions (user_id);

-- Trusted devices: the cookie carries a random value, the DB stores only
-- its sha256. A row buys 30 days without the 2FA challenge.
CREATE TABLE trusted_devices (
  id           UUID PRIMARY KEY,
  user_id      UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  token_hash   BYTEA NOT NULL UNIQUE,      -- sha256 of the cookie value
  label        TEXT NULL,
  user_agent   TEXT NULL,
  ip           INET NULL,
  created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
  last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  expires_at   TIMESTAMPTZ NOT NULL,
  revoked_at   TIMESTAMPTZ NULL
);

CREATE INDEX trusted_devices_user_idx ON trusted_devices (user_id);

-- Passkeys and security keys (WebAuthn). credential_id is unique per RP.
CREATE TABLE webauthn_credentials (
  id            UUID PRIMARY KEY,
  user_id       UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  credential_id BYTEA NOT NULL UNIQUE,
  public_key    BYTEA NOT NULL,
  sign_count    BIGINT NOT NULL DEFAULT 0,
  transports    TEXT[] NOT NULL DEFAULT '{}',
  aaguid        BYTEA NULL,
  nickname      TEXT NULL,
  created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
  last_used_at  TIMESTAMPTZ NULL
);

CREATE INDEX webauthn_credentials_user_idx ON webauthn_credentials (user_id);

-- TOTP: the shared secret is encrypted at rest with XChaCha20-Poly1305
-- under NAW_SECRET_KEY, never stored in the clear.
CREATE TABLE totp_credentials (
  user_id      UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  secret_enc   BYTEA NOT NULL,
  algorithm    TEXT NOT NULL DEFAULT 'SHA1',
  digits       SMALLINT NOT NULL DEFAULT 6,
  period       SMALLINT NOT NULL DEFAULT 30,
  confirmed_at TIMESTAMPTZ NULL,
  created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Recovery codes: 10 single-use spares, argon2id hashed like passwords.
CREATE TABLE recovery_codes (
  id         UUID PRIMARY KEY,
  user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  code_hash  BYTEA NOT NULL,
  used_at    TIMESTAMPTZ NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX recovery_codes_user_idx ON recovery_codes (user_id);

-- Former usernames. Renames and merges leave a redirect behind here.
CREATE TABLE user_aliases (
  alias      TEXT PRIMARY KEY,             -- lowercase, a former username
  user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Dev mail: the log backend stores every letter here so /dev/mailbox can
-- show the recent outbound mail without any SMTP infrastructure.
CREATE TABLE mail_outbox (
  id         UUID PRIMARY KEY,
  recipient  TEXT NOT NULL,
  subject    TEXT NOT NULL,
  body       TEXT NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- One row per notification actually sent, for audits and the digest job.
CREATE TABLE notification_log (
  id         UUID PRIMARY KEY,
  user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  kind       TEXT NOT NULL,
  channel    TEXT NOT NULL,                -- email | telegram | discord | inapp
  status     TEXT NOT NULL,                -- sent | failed | suppressed
  error      TEXT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX notification_log_user_idx ON notification_log (user_id, created_at DESC);

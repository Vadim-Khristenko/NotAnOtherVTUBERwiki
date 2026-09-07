-- NotAnotherWiki Engine, core schema (PLAN.md section 4)
-- The trigram extension must exist before anything uses it.

CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE TYPE page_namespace AS ENUM
  ('main','talk','template','module','file','user','project','category');

CREATE TYPE user_wiki_role AS ENUM
  ('registered','sponsor','moderator','admin','owner');

CREATE TABLE users (
  id                 UUID PRIMARY KEY,
  username           TEXT NOT NULL UNIQUE,
  email              TEXT NOT NULL UNIQUE,
  email_verified_at  TIMESTAMPTZ NULL,
  password_hash      TEXT NOT NULL,          -- PHC string, Argon2id, never logged
  global_role        TEXT NOT NULL DEFAULT 'registered',
  locale             TEXT NOT NULL DEFAULT 'en',
  timezone           TEXT NULL,
  created_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE wikis (
  id             UUID PRIMARY KEY,
  slug           TEXT NOT NULL UNIQUE,
  domain         TEXT NULL,
  name           TEXT NOT NULL,
  skin_id        UUID NULL,
  default_locale TEXT NOT NULL DEFAULT 'en',
  settings       JSONB NOT NULL DEFAULT '{}',
  created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE pages (
  id               UUID PRIMARY KEY,
  wiki_id          UUID NOT NULL REFERENCES wikis(id) ON DELETE CASCADE,
  namespace        page_namespace NOT NULL,
  slug             TEXT NOT NULL,
  title            TEXT NOT NULL,
  locale           TEXT NULL,
  layout_override  JSONB NULL,
  search_vector    tsvector,
  is_locked        BOOLEAN NOT NULL DEFAULT false,
  created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- current_revision_id is added after revisions exists, see the ALTER below.

CREATE TABLE revisions (
  id                    UUID PRIMARY KEY,
  page_id               UUID NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
  author_id             UUID NULL REFERENCES users(id) ON DELETE SET NULL,
  body_md               TEXT NOT NULL,
  content_hash          BYTEA NOT NULL,
  summary               TEXT NULL,
  is_minor              BOOLEAN NOT NULL DEFAULT false,
  is_patrolled          BOOLEAN NOT NULL DEFAULT false,
  reverted_revision_id  UUID NULL REFERENCES revisions(id) ON DELETE SET NULL,
  created_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Breaks the pages/revisions cycle.
ALTER TABLE pages
  ADD COLUMN current_revision_id UUID REFERENCES revisions(id) ON DELETE SET NULL;

CREATE TABLE wiki_memberships (
  user_id         UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  wiki_id         UUID NOT NULL REFERENCES wikis(id) ON DELETE CASCADE,
  role            user_wiki_role NOT NULL DEFAULT 'registered',
  sponsor_since   TIMESTAMPTZ NULL,
  sponsor_source  TEXT NULL,
  joined_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (user_id, wiki_id)
);

CREATE TABLE media (
  id           UUID PRIMARY KEY,
  wiki_id      UUID NOT NULL REFERENCES wikis(id) ON DELETE CASCADE,
  uploader_id  UUID NULL REFERENCES users(id) ON DELETE SET NULL,
  storage_key  TEXT NOT NULL,
  filename     TEXT NOT NULL,
  mime         TEXT NOT NULL,
  size_bytes   BIGINT NOT NULL DEFAULT 0,
  width        INT NULL,
  height       INT NULL,
  created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE categories (
  id        UUID PRIMARY KEY,
  wiki_id   UUID NOT NULL REFERENCES wikis(id) ON DELETE CASCADE,
  name      TEXT NOT NULL,
  slug      TEXT NOT NULL,
  parent_id UUID NULL REFERENCES categories(id) ON DELETE SET NULL
);

CREATE TABLE page_categories (
  page_id     UUID NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
  category_id UUID NOT NULL REFERENCES categories(id) ON DELETE CASCADE,
  PRIMARY KEY (page_id, category_id)
);

CREATE TABLE redirects (
  id          UUID PRIMARY KEY,
  wiki_id     UUID NOT NULL REFERENCES wikis(id) ON DELETE CASCADE,
  namespace   page_namespace NOT NULL,
  from_slug   TEXT NOT NULL,
  to_page_id  UUID NOT NULL REFERENCES pages(id) ON DELETE CASCADE
);

CREATE TABLE audit_log (
  id          UUID PRIMARY KEY,
  wiki_id     UUID NULL REFERENCES wikis(id) ON DELETE SET NULL,
  user_id     UUID NULL REFERENCES users(id) ON DELETE SET NULL,
  action      TEXT NOT NULL,
  entity_type TEXT NOT NULL,
  entity_id   UUID NULL,
  meta        JSONB NOT NULL DEFAULT '{}',  -- secrets never written here
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE render_cache (
  wiki_id          UUID NOT NULL REFERENCES wikis(id) ON DELETE CASCADE,
  content_hash     BYTEA NOT NULL,
  renderer_version INT NOT NULL,
  html             TEXT NOT NULL,
  created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (wiki_id, content_hash, renderer_version)
);

CREATE TABLE diagram_cache (
  wiki_id     UUID NOT NULL REFERENCES wikis(id) ON DELETE CASCADE,
  lang        TEXT NOT NULL,
  source_hash BYTEA NOT NULL,
  svg         TEXT NOT NULL,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (wiki_id, lang, source_hash)
);

CREATE TABLE page_template_deps (
  page_id          UUID NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
  template_id      UUID NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
  template_version INT NOT NULL,
  PRIMARY KEY (page_id, template_id)
);

CREATE TABLE components (
  id                   UUID PRIMARY KEY,
  wiki_id              UUID NOT NULL REFERENCES wikis(id) ON DELETE CASCADE,
  page_id              UUID NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
  name                 TEXT NOT NULL,
  flavor               TEXT NOT NULL DEFAULT 'vue-sfc',
  published_version_id UUID NULL,
  net_policy           JSONB NOT NULL DEFAULT '{}',
  created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE (wiki_id, name)
);

CREATE TABLE component_versions (
  id            UUID PRIMARY KEY,
  component_id  UUID NOT NULL REFERENCES components(id) ON DELETE CASCADE,
  version       INT NOT NULL,
  source        TEXT NOT NULL,
  bundle_ssr    TEXT NULL,
  bundle_client TEXT NULL,
  css           TEXT NULL,
  sri_ssr       TEXT NULL,
  sri_client    TEXT NULL,
  renderer_api  INT NOT NULL,
  status        TEXT NOT NULL,
  build_log     TEXT NULL,
  created_by    UUID NOT NULL REFERENCES users(id),
  created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE (component_id, version)
);

ALTER TABLE components
  ADD CONSTRAINT components_published_version_fk
  FOREIGN KEY (published_version_id) REFERENCES component_versions(id) ON DELETE SET NULL;

CREATE TABLE page_component_deps (
  page_id           UUID NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
  component_id      UUID NOT NULL REFERENCES components(id) ON DELETE CASCADE,
  component_version INT NOT NULL,
  PRIMARY KEY (page_id, component_id)
);

CREATE TABLE render_jobs (
  id           UUID PRIMARY KEY,
  wiki_id      UUID NOT NULL REFERENCES wikis(id) ON DELETE CASCADE,
  page_id      UUID NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
  content_hash BYTEA NOT NULL,
  priority     SMALLINT NOT NULL DEFAULT 0,
  attempts     INT NOT NULL DEFAULT 0,
  state        TEXT NOT NULL,
  error        TEXT NULL,
  enqueued_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  finished_at  TIMESTAMPTZ NULL
);

CREATE TABLE sessions (
  id         UUID PRIMARY KEY,
  user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  expires_at TIMESTAMPTZ NOT NULL,
  ip         INET NULL,
  user_agent TEXT NULL
);

CREATE TABLE access_tokens (
  id           UUID PRIMARY KEY,
  user_id      UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  name         TEXT NOT NULL,
  token_hash   BYTEA NOT NULL,
  scopes       TEXT[] NOT NULL,
  last_used_at TIMESTAMPTZ NULL,
  expires_at   TIMESTAMPTZ NULL,
  revoked_at   TIMESTAMPTZ NULL,
  created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE watchlist (
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  page_id UUID NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
  PRIMARY KEY (user_id, page_id)
);

CREATE TABLE webhooks (
  id         UUID PRIMARY KEY,
  wiki_id    UUID NOT NULL REFERENCES wikis(id) ON DELETE CASCADE,
  url        TEXT NOT NULL,
  secret     BYTEA NOT NULL,              -- never logged, never dumped
  events     TEXT[] NOT NULL,
  is_active  BOOLEAN NOT NULL DEFAULT true,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE webhook_deliveries (
  id              UUID PRIMARY KEY,
  webhook_id      UUID NOT NULL REFERENCES webhooks(id) ON DELETE CASCADE,
  event           TEXT NOT NULL,
  payload         JSONB NOT NULL,
  status_code     INT NULL,
  attempts        INT NOT NULL DEFAULT 0,
  next_attempt_at TIMESTAMPTZ NULL,
  created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX pages_uq
  ON pages (wiki_id, namespace, COALESCE(locale, ''), slug);

CREATE INDEX pages_search_idx ON pages USING gin (search_vector);

CREATE INDEX revisions_page_idx ON revisions (page_id, created_at DESC);

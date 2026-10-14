-- Which modules each page calls, rewritten on every save and by a reindex,
-- as template_uses is for templates. All pages counts a module's pages
-- from here.
CREATE TABLE module_uses (
  page_id     UUID NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
  wiki_id     UUID NOT NULL REFERENCES wikis(id) ON DELETE CASCADE,
  module_slug TEXT NOT NULL,
  PRIMARY KEY (page_id, module_slug)
);
CREATE INDEX module_uses_module_idx ON module_uses (wiki_id, module_slug);

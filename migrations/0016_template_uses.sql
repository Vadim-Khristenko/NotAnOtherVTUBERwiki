-- Which templates each page uses, rewritten on every save. A template's own
-- page lists the pages that use it from here. The render cache does not
-- depend on it: its key is the hash of the expanded text.
CREATE TABLE template_uses (
  page_id       UUID NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
  wiki_id       UUID NOT NULL REFERENCES wikis(id) ON DELETE CASCADE,
  template_slug TEXT NOT NULL,
  PRIMARY KEY (page_id, template_slug)
);
CREATE INDEX template_uses_template_idx ON template_uses (wiki_id, template_slug);

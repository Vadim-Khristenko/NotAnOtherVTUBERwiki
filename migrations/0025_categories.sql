-- Categories, as on MediaWiki: `[[Category:VTubers]]` in a page, or in a
-- template it uses, puts the page in the category. A category is known by
-- its key (`vtubers`, `витуберы`), its address is `/category:<key>`, and its
-- description is an ordinary page in the `category` namespace.
--
-- The first schema drafted categories as rows of their own with a parent;
-- nothing ever wrote those tables, and a category's parents are now the
-- categories its own description page is in.
DROP TABLE IF EXISTS page_categories;
DROP TABLE IF EXISTS categories;

-- Which categories each page is in, rewritten on every save and whenever a
-- view finds the page's expanded text says otherwise.
CREATE TABLE page_categories (
  page_id  UUID NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
  wiki_id  UUID NOT NULL REFERENCES wikis (id) ON DELETE CASCADE,
  category TEXT NOT NULL,
  -- The name as the page wrote it, for a category with no description yet.
  name     TEXT NOT NULL,
  -- What the page sorts under in the category; its title when NULL.
  sort_key TEXT NULL,
  PRIMARY KEY (page_id, category)
);
CREATE INDEX page_categories_category_idx ON page_categories (wiki_id, category);

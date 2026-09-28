-- Page templates get a namespace of their own: /page-template:vtuber-article.
-- A page template lays out a whole new page and is translated like an
-- article; a template (/template:...) is a component a page calls. Mixing
-- them in one namespace let one be edited by the rules of the other.
--
-- A new enum value cannot be used in the transaction that adds it, so the
-- existing starters move in the next migration.
ALTER TYPE page_namespace ADD VALUE IF NOT EXISTS 'page_template';

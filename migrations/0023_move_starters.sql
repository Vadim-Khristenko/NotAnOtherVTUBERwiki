-- Every template whose text starts with `<!-- starter: Label -->` was a page
-- template: it moves to the page_template namespace, same slug, same history.
-- Links to /template:<slug> keep working: the engine redirects them.
UPDATE pages p
SET namespace = 'page_template'
FROM revisions r
WHERE r.id = p.current_revision_id
  AND p.namespace = 'template'
  AND r.body_md LIKE '<!-- starter:%';

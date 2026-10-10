-- Modules get a namespace of their own: /module:stats. A module is a page
-- whose ```js block holds code the server runs when another page calls
-- :::Module:Stats:summary; the rest of the page is its documentation. Code
-- runs on the server, so only admins edit this namespace.
ALTER TYPE page_namespace ADD VALUE IF NOT EXISTS 'module';

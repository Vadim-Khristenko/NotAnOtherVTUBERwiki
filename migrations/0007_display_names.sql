-- NotAnotherWiki Engine, display names.
--
-- The username stays the identifier (lowercase, in URLs, in mentions). The
-- display name is what people read: any script, cleaned of layout breaking
-- characters by the application before it is written. NULL shows the username.

ALTER TABLE users ADD COLUMN display_name TEXT NULL;

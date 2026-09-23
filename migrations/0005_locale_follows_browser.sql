-- NotAnotherWiki Engine, "no language chosen" is a value of its own.
--
-- users.locale defaulted to 'en', and a signed-in account's language outranks
-- the browser's, so every account an admin created showed English to people
-- whose browser asks for Russian. An empty string now means "follow the
-- browser". Nobody could pick a language before the settings page existed, so
-- every 'en' here is the old default and not a choice.

ALTER TABLE users ALTER COLUMN locale SET DEFAULT '';
UPDATE users SET locale = '' WHERE locale = 'en';

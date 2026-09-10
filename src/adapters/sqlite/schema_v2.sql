ALTER TABLE sources ADD COLUMN parse_notices TEXT NOT NULL DEFAULT '[]';

PRAGMA user_version = 2;

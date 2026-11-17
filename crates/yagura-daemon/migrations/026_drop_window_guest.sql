-- The column migration 022 added has no writer or reader.
ALTER TABLE windows DROP COLUMN guest;

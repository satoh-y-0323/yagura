-- Where a window's wake file is, as its open settled it, for a window whose
-- hooks were injected with an answer naming its wake paths. A row without
-- one was written before the record was kept.
ALTER TABLE windows ADD COLUMN wake TEXT;

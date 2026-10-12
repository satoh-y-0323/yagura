ALTER TABLE plan ADD COLUMN review TEXT CHECK (review IN ('pending', 'approved'));
ALTER TABLE plan ADD COLUMN version INTEGER NOT NULL DEFAULT 0;
ALTER TABLE plan_task ADD COLUMN review_mark TEXT CHECK (review_mark IN ('new', 'changed'));

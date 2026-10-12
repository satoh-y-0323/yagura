ALTER TABLE plan_task ADD COLUMN report_outcome TEXT CHECK (report_outcome IN ('succeeded', 'failed'));
ALTER TABLE plan_task ADD COLUMN report_summary TEXT;
ALTER TABLE plan_task ADD COLUMN report_path TEXT;
ALTER TABLE plan_task ADD COLUMN reported_at INTEGER;
